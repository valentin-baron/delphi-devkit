import { commands, ConfigurationTarget, env, Uri, window, workspace, Disposable } from "vscode";
import { COMMANDS, PROJECTS } from "./constants";
import { join } from "path";
import { promises as fs } from 'fs';
import { Runtime } from "./runtime";
import { ExtensionDataExport } from "./types";
import { Entities } from "./db/entities";

export class GeneralCommands {
  public static get registers(): Disposable[] {
    return [
      commands.registerCommand(COMMANDS.EXPORT_CONFIGURATION, this.exportConfiguration.bind(this)),
      commands.registerCommand(COMMANDS.IMPORT_CONFIGURATION, this.importConfiguration.bind(this))
    ];
  }

  private static async exportConfiguration(): Promise<void> {
    const fileUri = await window.showSaveDialog({
      saveLabel: 'Export DDK',
      title: 'Export DDK Configuration',
      filters: {
        'DDK JSON files': ['ddk.json'],
        'All files': ['*']
      },
      defaultUri: Uri.file(join(env.appRoot, 'configuration.ddk.json'))
    });
    if (!fileUri) return;
    try {
      const config = Runtime.configEntity;
      const data = new ExtensionDataExport.FileContent(config, Runtime.compilerConfigurations);
      await fs.writeFile(fileUri.fsPath, JSON.stringify(data, null, 2), 'utf8');
      window.showInformationMessage('Configuration exported successfully.');
    } catch (error) {
      window.showErrorMessage(`Failed to export configuration: ${error}`);
    }
  }

  private static async importConfigurationV1_0(data: ExtensionDataExport.FileContent): Promise<void> {
    await Runtime.db.clear();
    await Runtime.db.save(Entities.Configuration.clone(data.configuration));
    if (data.compilers)
      await workspace
        .getConfiguration(PROJECTS.CONFIG.KEY)
        .update(PROJECTS.CONFIG.COMPILER.CONFIGURATIONS, data.compilers || [], ConfigurationTarget.Global);
  }

  private static async importConfiguration(): Promise<void> {
    const fileUri = (
      await window.showOpenDialog({
        canSelectMany: false,
        title: 'Import DDK Configuration',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Import',
        filters: {
          'DDK JSON files': ['ddk.json'],
          'All files': ['*']
        }
      })
    )?.[0];
    if (!fileUri) return;
    try {
      const content = await fs.readFile(fileUri.fsPath, 'utf8');
      const data = JSON.parse(content) as ExtensionDataExport.FileContent;
      if (data) {
        switch (data.version as ExtensionDataExport.Version) {
          case ExtensionDataExport.Version.V1_0:
            await this.importConfigurationV1_0(data);
            break;
          default:
            window.showErrorMessage(`Unsupported configuration version: ${data.version}`);
            return;
        }
        await Runtime.projects.workspacesTreeView.refresh();
        await Runtime.projects.groupProjectTreeView.refresh();
        await Runtime.projects.compilerStatusBarItem.updateDisplay();
        window.showInformationMessage('Configuration imported successfully.');
      } else window.showErrorMessage('Invalid configuration file.');
    } catch (error) {
      window.showErrorMessage(`Failed to import configuration: ${error}`);
    }
  }
}