import { commands, Disposable, window } from 'vscode';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { BaseFileItem } from '../projects/trees/items/baseFile';

export class DelphiLspCommands {
  public static get registers(): Disposable[] {
    return [commands.registerCommand(DELPHILSP.COMMAND.GENERATE_CONFIG, this.generateConfig.bind(this))];
  }

  /** `item` is set when invoked from the tree context menu; without it
   *  (Command Palette, keybinding) the server targets the active project. */
  private static async generateConfig(item?: BaseFileItem): Promise<void> {
    const project = item?.project?.entity;
    const label = project?.name ?? 'the active project';

    try {
      const result = await Runtime.client.generateDelphiLspConfig(project ? String(project.id) : undefined);
      if (result.warnings.length > 0)
        window.showWarningMessage(`DelphiLSP config for "${label}" generated with warnings:\n${result.warnings.join('\n')}`);
      window.showInformationMessage(`Wrote DelphiLSP settings for "${label}": ${result.file_path}`);
    } catch (error) {
      window.showErrorMessage(`Failed to generate DelphiLSP settings for "${label}": ${error}`);
    }
  }
}
