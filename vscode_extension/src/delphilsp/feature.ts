import { extensions, workspace } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { DelphiLspCommands } from './commands';
import { DelphiLspAutoSync } from './autoSync';
import { MergedDiagnostics } from './mergedDiagnostics';

/**
 * Integrates DDK with Embarcadero's DelphiLSP VS Code extension: generates
 * `.delphilsp.json` settings files from DDK's own project state and keeps
 * DelphiLSP's active `settingsFile` pointed at whichever project DDK has
 * selected.
 *
 * Everything here is inert unless DelphiLSP (`embarcaderotechnologies.delphilsp`)
 * is installed — this is the only feature that is entirely optional based on
 * another extension's presence, so it is gated behind its own availability
 * flag rather than an `extensionDependencies` entry (DDK is fully useful
 * without DelphiLSP).
 */
export class DelphiLspFeature implements Feature {
  private extensionAvailable = false;

  /** The DelphiLSP extension is installed. Mirrored into the
   *  `ddk:delphiLspAvailable` context key, which gates the commands'
   *  `when`/`enablement` clauses. */
  public get isDelphiLspExtensionAvailable(): boolean {
    return this.extensionAvailable;
  }

  /** Whether the auto-sync may generate settings files and repoint
   *  DelphiLSP: the extension must be installed AND the user opted in via
   *  the `autoSync` setting. */
  public get canAutoGenerate(): boolean {
    if (!this.extensionAvailable) return false;
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.AUTO_SYNC, true);
  }

  public async initialize(): Promise<void> {
    // Always initialized: it is the rendering route for the server's compile
    // diagnostics; the dedup against DelphiLSP is gated internally.
    MergedDiagnostics.initialize();
    this.updateAvailability();
    Runtime.extension.subscriptions.push(
      ...DelphiLspCommands.registers,
      // DelphiLSP may be installed (or uninstalled) after DDK has already activated.
      extensions.onDidChange(() => this.updateAvailability())
    );
  }

  /** Called whenever DDK's project state is (re)loaded, so the active project
   *  can be compared against the last one auto-synced. Cheap no-op when the
   *  feature is unavailable or the active project has not actually changed. */
  public async onProjectsUpdated(): Promise<void> {
    await DelphiLspAutoSync.onProjectsUpdated();
  }

  private updateAvailability(): void {
    const available = !!extensions.getExtension(DELPHILSP.EXTENSION_ID);
    if (available === this.extensionAvailable) return;
    this.extensionAvailable = available;
    Runtime.setContext(DELPHILSP.CONTEXT.AVAILABLE, available);
  }
}
