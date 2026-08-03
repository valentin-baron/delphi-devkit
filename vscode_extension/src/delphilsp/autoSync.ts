import { createHash } from 'crypto';
import { promises as fs } from 'fs';
import { basename, dirname, join } from 'path';
import { ConfigurationTarget, languages, Uri, window, workspace } from 'vscode';
import { Runtime } from '../runtime';
import { Entities } from '../projects/entities';
import { DELPHILSP } from '../constants';
import { Option } from '../types';
import { basenameNoExt, fileExists } from '../utils';

/**
 * Keeps DelphiLSP's active `settingsFile` pointed at DDK's active project.
 *
 * Runs whenever `DelphiLspFeature.onProjectsUpdated` is invoked (project
 * state reload / update notification); it only actually does anything the
 * first time it sees a given `active_project_id`, so unrelated project
 * updates (compile results, discovery, …) don't cause repeated churn.
 */
export namespace DelphiLspAutoSync {
  // `undefined` means "never observed yet" — distinct from `null`/no active project,
  // so the very first project selection after activation still triggers a sync.
  let lastSyncedProjectId: Option<number> = undefined;

  /** `<dir>\<stem>.delphilsp.json` next to the project's `.dpr`/`.dpk` main
   *  source — replicates `delphilsp::default_out_path` in `core`. Deliberately
   *  keyed off the main source's own directory rather than `project.directory`,
   *  since that is what the generator itself writes next to. */
  function expectedSettingsFilePath(project: Entities.Project): Option<string> {
    const mainSource = project.dpr || project.dpk;
    if (!mainSource) return undefined;
    return join(dirname(mainSource), `${basenameNoExt(mainSource)}.delphilsp.json`);
  }

  interface SettingsFileMarkers {
    generatedBy?: string;
    dprojHash?: string;
  }

  async function readSettingsFileMarkers(filePath: string): Promise<SettingsFileMarkers> {
    try {
      const content = await fs.readFile(filePath, 'utf8');
      const parsed = JSON.parse(content);
      return {
        generatedBy: typeof parsed?.generatedBy === 'string' ? parsed.generatedBy : undefined,
        dprojHash: typeof parsed?.dprojHash === 'string' ? parsed.dprojHash : undefined,
      };
    } catch {
      // Unreadable or not valid JSON — treat as "not ours", same as an IDE-generated file.
      return {};
    }
  }

  /** A DDK-owned settings file is stale once the `.dproj` it was derived from
   *  has different content: its stored `dprojHash` (SHA-256 of the dproj
   *  bytes, written by the generator in `core`) no longer matches. Content is
   *  compared instead of timestamps because mtimes are unreliable on Windows
   *  and a rewritten-but-identical `.dproj` must not trigger a regeneration.
   *  A DDK file without the hash predates it — regenerate once to stamp it.
   *  Projects with no `.dproj` (bare `.dpr`/`.dpk`) have nothing to compare
   *  against, so an existing file is always kept. */
  async function isStale(markers: SettingsFileMarkers, project: Entities.Project): Promise<boolean> {
    if (!project.dproj) return false;
    if (!markers.dprojHash) return true;
    try {
      const dprojBytes = await fs.readFile(project.dproj);
      return createHash('sha256').update(dprojBytes).digest('hex') !== markers.dprojHash;
    } catch {
      return false;
    }
  }

  /** Ensures the expected `.delphilsp.json` exists and is current, generating
   *  it through the server when needed. Returns the file path to point
   *  DelphiLSP at, or `undefined` when nothing could be determined/generated. */
  async function ensureSettingsFile(project: Entities.Project): Promise<Option<string>> {
    const filePath = expectedSettingsFilePath(project);
    if (!filePath) return undefined;

    let needsGeneration = !(await fileExists(filePath));
    if (!needsGeneration) {
      const markers = await readSettingsFileMarkers(filePath);
      if (markers.generatedBy === DELPHILSP.GENERATED_BY_MARKER)
        needsGeneration = await isStale(markers, project);
      // Existing file without our marker was produced by the RAD Studio IDE — leave it untouched.
    }
    if (!needsGeneration) return filePath;

    try {
      const result = await Runtime.client.generateDelphiLspConfig(String(project.id));
      if (result.warnings.length > 0)
        console.warn(`[DDK] DelphiLSP config for "${project.name}" generated with warnings:\n${result.warnings.join('\n')}`);
      return result.file_path;
    } catch (error) {
      console.error(`[DDK] Failed to auto-generate DelphiLSP config for "${project.name}": ${error}`);
      window.showWarningMessage(`DDK: Failed to generate DelphiLSP settings for "${project.name}": ${error}`);
      return undefined;
    }
  }

  async function pointDelphiLspAt(filePath: string): Promise<void> {
    const uri = Uri.file(filePath).toString();
    const config = workspace.getConfiguration(DELPHILSP.EXTERNAL_SETTINGS.SECTION);
    if (config.get<string>(DELPHILSP.EXTERNAL_SETTINGS.SETTINGS_FILE) === uri) return;

    const target = (workspace.workspaceFolders?.length ?? 0) > 0 ? ConfigurationTarget.Workspace : ConfigurationTarget.Global;
    try {
      await config.update(DELPHILSP.EXTERNAL_SETTINGS.SETTINGS_FILE, uri, target);
      // Mirror DelphiLSP's own "Loaded project …" toast so the silent auto-switch is visible.
      window.showInformationMessage(`DDK: DelphiLSP settings switched to ${basename(filePath)}`);
      await revalidateOpenDocuments();
    } catch (error) {
      console.error(`[DDK] Failed to update DelphiLSP's settingsFile: ${error}`);
    }
  }

  /** How long the DelphiLSP server is given to load the pushed settings
   *  (its client expands and forwards them right after the update) before
   *  the open editors are re-opened against the new project context. */
  const CONFIG_LOAD_GRACE_MS = 1500;

  function isRevalidateEnabled(): boolean {
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.REVALIDATE_ON_SWITCH, true);
  }

  function isDelphiSource(fsPath: string): boolean {
    return /\.(pas|dpr|dpk)$/i.test(fsPath);
  }

  /**
   * DelphiLSP's server applies a `settingsFile` change to FUTURE validations
   * but never spontaneously re-validates documents that are already open —
   * not on the configuration push, and (verified via its raw LSP logs) not
   * even after a full server restart with `didOpen` replay; its own "Select
   * project settings" command has the same limitation, leaving stale
   * diagnostics around until each file is edited. The only trigger it honors
   * is a `didOpen` arriving AFTER the new settings are loaded, so: wait for
   * the pushed settings to land, then briefly flip each open Delphi
   * document's language (objectpascal → plaintext → back). The language flip
   * makes VS Code re-emit `didClose`/`didOpen` for the document — which the
   * LSP client relays, triggering a re-validation under the new project —
   * while the editor tab, focus, cursor, dirty state and undo history stay
   * completely untouched (it is the same text buffer; only its language
   * label round-trips, with a barely visible syntax-highlight blink).
   */
  async function revalidateOpenDocuments(): Promise<void> {
    if (!isRevalidateEnabled()) return;

    const delphiDocuments = workspace.textDocuments.filter((doc) => doc.uri.scheme === 'file' && isDelphiSource(doc.fileName));
    if (delphiDocuments.length === 0) return;

    await new Promise((resolve) => setTimeout(resolve, CONFIG_LOAD_GRACE_MS));
    for (const document of delphiDocuments)
      try {
        // Temporary language round-trip, NOT a cosmetic accident: changing a
        // document's language is the only VS Code API that re-emits
        // didClose/didOpen for an open document without touching the editor
        // (tab, focus, cursor, dirty flag, undo stack all survive — it is the
        // same text buffer). The didOpen this produces is what finally makes
        // DelphiLSP re-validate the file under the just-switched project;
        // nothing else works: the server ignores configuration pushes for
        // already-open files and even a full server restart re-plays didOpen
        // BEFORE the settings arrive (verified via its raw LSP logs).
        const originalLanguage = document.languageId;
        const reopened = await languages.setTextDocumentLanguage(document, 'plaintext');
        await languages.setTextDocumentLanguage(reopened, originalLanguage);
      } catch (error) {
        console.error(`[DDK] Failed to re-validate ${document.fileName} for DelphiLSP: ${error}`);
      }
  }

  export async function onProjectsUpdated(): Promise<void> {
    if (!Runtime.delphilsp?.canAutoGenerate) return;

    const activeId = Runtime.projectsData?.active_project_id ?? undefined;
    if (activeId === lastSyncedProjectId) return;
    lastSyncedProjectId = activeId;
    if (!activeId) return;

    const project = Runtime.projectsData?.projects.find((p) => p.id === activeId);
    if (!project) return;

    const filePath = await ensureSettingsFile(project);
    if (filePath) await pointDelphiLspAt(filePath);
  }
}
