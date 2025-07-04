import { window, StatusBarAlignment, StatusBarItem, workspace, commands, QuickPickItem } from 'vscode';
import { Compiler, CompilerConfiguration } from './Compiler';

/**
 * Manages the compiler selection status bar item with dropdown functionality
 */
export class CompilerStatusBar {
  private statusBarItem: StatusBarItem;
  private static instance: CompilerStatusBar;

  private constructor() {
    // Create status bar item aligned to the left with priority 100 (similar to Run and Debug)
    this.statusBarItem = window.createStatusBarItem('delphiCompiler', StatusBarAlignment.Left, 100);
    this.statusBarItem.command = 'delphi-utils.showCompilerDropdown';
    this.statusBarItem.tooltip = 'Select Delphi Compiler Configuration';

    // Initialize the display
    this.updateDisplay();

    // Show only when workspace is open
    if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
      this.statusBarItem.show();
    }

    // Listen for configuration changes
    workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('delphi-utils.compiler')) {
        this.updateDisplay();
      }
    });

    // Listen for workspace changes
    workspace.onDidChangeWorkspaceFolders(() => {
      if (workspace.workspaceFolders && workspace.workspaceFolders.length > 0) {
        this.statusBarItem.show();
      } else {
        this.statusBarItem.hide();
      }
    });
  }

  /**
   * Get or create the singleton instance
   */
  static getInstance(): CompilerStatusBar {
    if (!CompilerStatusBar.instance) {
      CompilerStatusBar.instance = new CompilerStatusBar();
    }
    return CompilerStatusBar.instance;
  }

  /**
   * Update the status bar display with current compiler
   */
  private async updateDisplay(): Promise<void> {
    try {
      const config = workspace.getConfiguration('delphi-utils.compiler');
      const currentConfigName: string = config.get('currentConfiguration', 'Delphi 12');

      // Set the text with an icon similar to Run and Debug
      this.statusBarItem.text = `$(tools) ${currentConfigName}`;
    } catch (error) {
      this.statusBarItem.text = '$(tools) No Compiler';
      console.error('Error updating compiler status bar:', error);
    }
  }

  /**
   * Show the compiler selection dropdown
   */
  async showDropdown(): Promise<void> {
    try {
      const configurations = Compiler.getAvailableConfigurations();

      if (configurations.length === 0) {
        window.showErrorMessage('No compiler configurations found. Please configure Delphi compiler settings.');
        return;
      }

      const config = workspace.getConfiguration('delphi-utils.compiler');
      const currentConfigName: string = config.get('currentConfiguration', 'Delphi 12');

      // Create quick pick items with additional information
      const items: QuickPickItem[] = configurations.map(config => {
        const isCurrentConfig = config.name === currentConfigName;
        return {
          label: isCurrentConfig ? `$(check) ${config.name}` : config.name,
          description: config.rsVarsPath,
          detail: `MSBuild: ${config.msBuildPath}`,
          picked: isCurrentConfig
        };
      });

      // Add a separator and settings option
      items.push(
        { label: '', kind: -1 } as QuickPickItem, // Separator
        {
          label: '$(gear) Configure Compiler Settings...',
          description: 'Open VS Code settings to manage compiler configurations'
        }
      );

      const selected = await window.showQuickPick(items, {
        placeHolder: 'Select Delphi Compiler Configuration',
        matchOnDescription: true,
        matchOnDetail: true,
        title: 'Delphi Compiler',
        ignoreFocusOut: false
      });

      if (selected) {
        if (selected.label.includes('Configure Compiler Settings')) {
          // Open settings for compiler configuration
          await commands.executeCommand('workbench.action.openSettings', 'delphi-utils.compiler.configurations');
        } else if (selected.label && !selected.label.includes('$(gear)')) {
          // Remove the check mark if present and set the selected configuration
          const configName = selected.label.replace('$(check) ', '');
          await Compiler.setCurrentConfiguration(configName);
          this.updateDisplay();
        }
      }
    } catch (error) {
      window.showErrorMessage(`Failed to show compiler dropdown: ${error}`);
    }
  }

  /**
   * Register commands for the status bar
   */
  static registerCommands() {
    return [
      commands.registerCommand('delphi-utils.showCompilerDropdown', () => {
        CompilerStatusBar.getInstance().showDropdown();
      })
    ];
  }

  /**
   * Initialize the status bar (call this from extension activation)
   */
  static initialize(): CompilerStatusBar {
    return CompilerStatusBar.getInstance();
  }

  /**
   * Dispose of the status bar item
   */
  dispose(): void {
    this.statusBarItem.dispose();
  }
}
