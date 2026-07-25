import { commands, Disposable, window } from 'vscode';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { BaseFileItem } from '../projects/trees/items/baseFile';
import { assertError } from '../utils';

export class DelphiLspCommands {
  public static get registers(): Disposable[] {
    return [commands.registerCommand(DELPHILSP.COMMAND.GENERATE_CONFIG, this.generateConfig.bind(this))];
  }

  private static async generateConfig(item: BaseFileItem): Promise<void> {
    const project = item?.project?.entity;
    if (!assertError(project, 'Could not determine project for the selected item.')) return;

    try {
      const result = await Runtime.client.generateDelphiLspConfig(String(project.id));
      if (result.warnings.length > 0)
        window.showWarningMessage(`DelphiLSP config for "${project.name}" generated with warnings:\n${result.warnings.join('\n')}`);
      window.showInformationMessage(`Wrote DelphiLSP settings for "${project.name}": ${result.file_path}`);
    } catch (error) {
      window.showErrorMessage(`Failed to generate DelphiLSP settings for "${project.name}": ${error}`);
    }
  }
}
