import { window, StatusBarAlignment, StatusBarItem, workspace } from 'vscode';
import { Runtime } from '../../runtime';
import { PROJECTS } from '../../constants';

export class CompilerPicker {
  private statusBarItem: StatusBarItem;

  constructor() {
    // Create status bar item aligned to the left with priority 100 (similar to Run and Debug)
    this.statusBarItem = window.createStatusBarItem(
      PROJECTS.STATUS_BAR.COMPILER,
      StatusBarAlignment.Left,
      0
    );
    this.statusBarItem.command = PROJECTS.COMMAND.SELECT_COMPILER;
    this.statusBarItem.tooltip = 'Select Delphi Compiler Configuration';

    // Initialize the display
    this.updateDisplay();

    // Show only when workspace is open
    if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
      this.statusBarItem.show();
    }

    Runtime.extension.subscriptions.push(this.statusBarItem);
  }

  public async updateDisplay(): Promise<void> {
    try {
      const configuration = await Runtime.db.getConfiguration();
      const currentConfigName = configuration.groupProjectsCompiler || 'No Compiler';
      this.statusBarItem.text = `$(tools) .groupproj Compiler: ${currentConfigName}`;
    } catch (error) {
      this.statusBarItem.text = '$(tools) No .groupproj Compiler';
    }
  }
}
