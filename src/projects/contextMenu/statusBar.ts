import { window, StatusBarAlignment, StatusBarItem, workspace } from 'vscode';
import { Runtime } from '../../runtime';
import { Projects } from '../../constants';

export class CompilerPicker {
  private statusBarItem: StatusBarItem;

  constructor() {
    // Create status bar item aligned to the left with priority 100 (similar to Run and Debug)
    this.statusBarItem = window.createStatusBarItem('delphiCompiler', StatusBarAlignment.Left, 100);
    this.statusBarItem.command = 'delphi-devkit.selectCompilerConfiguration';
    this.statusBarItem.tooltip = 'Select Delphi Compiler Configuration';

    // Initialize the display
    this.updateDisplay();

    // Show only when workspace is open
    if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
      this.statusBarItem.show();
    }

    // Listen for configuration changes
    workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('delphi-devkit.compiler')) {
        this.updateDisplay();
      }
    });

    // Listen for workspace changes
    Runtime.subscribe(() => {
      if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
        this.statusBarItem.show();
      } else {
        this.statusBarItem.hide();
      }
    });
    Runtime.extension.subscriptions.push(this.statusBarItem);
  }

  private async updateDisplay(): Promise<void> {
    try {
      const config = workspace.getConfiguration(Projects.Config.Key);
      const currentConfigName: string = config.get(
        Projects.Config.Compiler.CurrentConfiguration, 
        Projects.Config.Compiler.Value_DefaultConfiguration
      );

      // Set the text with an icon similar to Run and Debug
      this.statusBarItem.text = `$(tools) ${currentConfigName}`;
    } catch (error) {
      this.statusBarItem.text = '$(tools) No Compiler';
      console.error('Error updating compiler status bar:', error);
    }
  }
}
