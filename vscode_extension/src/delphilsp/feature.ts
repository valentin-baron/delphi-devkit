import { extensions, workspace } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { DelphiLspCommands } from './commands';
import { DelphiLspAutoSync } from './autoSync';
import { DelphiLspGitExclude } from './gitExclude';
import { MergedDiagnostics } from './mergedDiagnostics';

/**
 * Generates `.delphilsp.json` settings files from DDK's project state and keeps
 * Embarcadero's DelphiLSP extension pointed at the selected project. Inert
 * unless DelphiLSP is installed — gated behind an availability flag rather than
 * `extensionDependencies`, since DDK is fully useful without it.
 */
export class DelphiLspFeature implements Feature {
  private extensionAvailable = false;

  /** Mirrored into the `ddk:delphiLspAvailable` context key, which gates the
   *  commands' `when`/`enablement` clauses. */
  public get isDelphiLspExtensionAvailable(): boolean {
    return this.extensionAvailable;
  }

  /** The auto-sync needs both the extension installed and the `autoSync`
   *  setting opted in. */
  public get canAutoGenerate(): boolean {
    if (!this.extensionAvailable) return false;
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.AUTO_SYNC, true);
  }

  public async initialize(): Promise<void> {
    // Always: it is the rendering route for the server's compile diagnostics,
    // and the dedup against DelphiLSP is gated internally.
    MergedDiagnostics.initialize();
    this.updateAvailability();
    Runtime.extension.subscriptions.push(
      ...DelphiLspCommands.registers,
      // DelphiLSP may be installed (or uninstalled) after DDK has already activated.
      extensions.onDidChange(() => this.updateAvailability()),
      workspace.onDidChangeConfiguration((event) => {
        if (event.affectsConfiguration(`${DELPHILSP.CONFIG.KEY}.${DELPHILSP.CONFIG.AUTO_IGNORE}`))
          void DelphiLspGitExclude.onSettingChanged();
      })
    );
  }

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
