import { createHash } from 'crypto';
import { promises as fs } from 'fs';
import { basename, dirname, join } from 'path';
import { ConfigurationTarget, languages, Uri, window, workspace } from 'vscode';
import { Runtime } from '../runtime';
import { Entities } from '../projects/entities';
import { DELPHILSP } from '../constants';
import { Option } from '../types';
import { basenameNoExt, fileExists } from '../utils';
import { DelphiLspGitExclude } from './gitExclude';

/**
 * Keeps DelphiLSP's active `settingsFile` pointed at DDK's active project.
 * Runs on every project-state update but acts only when `active_project_id`
 * changes, so compile results and discovery cause no churn.
 */
export namespace DelphiLspAutoSync {
  // `undefined` is "never observed yet", distinct from no active project, so the
  // first project selection after activation still triggers a sync.
  let lastSyncedProjectId: Option<number> = undefined;

  /** `<dir>\<stem>.delphilsp.json` next to the project's `.dpr`/`.dpk` main
   *  source, replicating `delphilsp::default_out_path` in `core`. Keyed off the
   *  main source's directory, not `project.directory`: that is where the
   *  generator itself writes. */
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

  /** Stale once the stored `dprojHash` (SHA-256 of the dproj bytes, written by
   *  the generator in `core`) no longer matches. Content, not mtime: mtimes are
   *  unreliable on Windows and a rewritten-but-identical `.dproj` must not
   *  regenerate. A file without the hash predates it; a project without a
   *  `.dproj` has nothing to compare, so its file is always kept. */
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

  /** The `.delphilsp.json` to point DelphiLSP at, generated through the server
   *  when missing or stale; `undefined` when it could not be. */
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

  /** How long the DelphiLSP server is given to load the pushed settings before
   *  the open editors are re-opened against the new project context. */
  const CONFIG_LOAD_GRACE_MS = 1500;

  function isRevalidateEnabled(): boolean {
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.REVALIDATE_ON_SWITCH, true);
  }

  function isDelphiSource(fsPath: string): boolean {
    return /\.(pas|dpr|dpk)$/i.test(fsPath);
  }

  /**
   * DelphiLSP applies a `settingsFile` change to future validations only; the
   * sole trigger for an already-open document is a `didOpen` arriving AFTER the
   * new settings are loaded (a server restart replays `didOpen` before them).
   * So wait for the settings to land, then flip each open Delphi document's
   * language and back: that is the only VS Code API re-emitting
   * `didClose`/`didOpen` on the same buffer, leaving tab, focus, cursor, dirty
   * state and undo history untouched. DelphiLSP's own "Select project
   * settings" command has the same limitation (verified in its raw LSP logs),
   * leaving stale diagnostics until each file is edited.
   */
  async function revalidateOpenDocuments(): Promise<void> {
    if (!isRevalidateEnabled()) return;

    const delphiDocuments = workspace.textDocuments.filter((doc) => doc.uri.scheme === 'file' && isDelphiSource(doc.fileName));
    if (delphiDocuments.length === 0) return;

    await new Promise((resolve) => setTimeout(resolve, CONFIG_LOAD_GRACE_MS));
    for (const document of delphiDocuments)
      try {
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
    if (!filePath) return;
    await DelphiLspGitExclude.ensureExcludedFor(filePath);
    await pointDelphiLspAt(filePath);
  }
}
