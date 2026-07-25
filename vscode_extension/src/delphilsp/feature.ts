import { extensions } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { DelphiLspCommands } from './commands';
import { DelphiLspAutoSync } from './autoSync';

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
  private _available = false;

  public get isAvailable(): boolean {
    return this._available;
  }

  public async initialize(): Promise<void> {
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
    if (available === this._available) return;
    this._available = available;
    Runtime.setContext(DELPHILSP.CONTEXT.AVAILABLE, available);
  }
}
