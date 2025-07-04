import { Uri, workspace, window, Terminal, ThemeIcon } from 'vscode';
import { basename, dirname, join } from 'path';

/**
 * Configuration for a Delphi compiler
 */
export interface CompilerConfiguration {
  name: string;
  rsVarsPath: string;
  msBuildPath: string;
  buildArguments: string[];
}

/**
 * Delphi project compiler using PowerShell script
 */
export class Compiler {
  private static terminal: Terminal | undefined;

  /**
   * Get or create the Delphi compilation terminal
   */
  private static getCompilerTerminal(): Terminal {
    if (!this.terminal || this.terminal.exitStatus !== undefined) {
      this.terminal = window.createTerminal({
        name: 'Delphi Compiler',
        iconPath: new ThemeIcon('tools')
      });
    }
    return this.terminal;
  }

  /**
   * Compile or recreate a Delphi project
   * @param dprojPath Path to the .dproj file
   * @param recreate Whether to recreate (clean + build) or just compile (clean + make)
   */
  static async compile(dprojPath: Uri, recreate: boolean = false): Promise<void> {
    try {
      // Get the current compiler configuration
      const config = await this.getCurrentCompilerConfiguration();
      if (!config) {
        window.showErrorMessage('No compiler configuration found. Please configure Delphi compiler settings.');
        return;
      }

      // Extract file information
      const fileName = basename(dprojPath.fsPath);
      const projectDir = dirname(dprojPath.fsPath);

      // Get relative path for description
      const relativePath = workspace.asRelativePath(projectDir);
      const pathDescription = relativePath === projectDir ? projectDir : relativePath;

      // Determine action and build target
      const actionDescription = recreate ? 'recreate (clean + build)' : 'compile (clean + make)';
      const buildTarget = recreate ? 'Build' : 'Make';

      // Prepare build arguments with target
      const buildArguments = [
        `/t:Clean,${buildTarget}`,
        ...config.buildArguments
      ];

      // Get the PowerShell script path - it's copied to the dist folder during build
      const scriptPath = join(__dirname, 'compile.ps1');

      // Join build arguments into a single string
      const buildArgumentsString = buildArguments.join(' ');

      // Prepare PowerShell command arguments - pass build arguments as single string
      const psArgs = [
        '-ExecutionPolicy', 'Bypass',
        '-File', `"${scriptPath}"`,
        '-ProjectPath', `"${dprojPath.fsPath}"`,
        '-RSVarsPath', `"${config.rsVarsPath}"`,
        '-MSBuildPath', `"${config.msBuildPath}"`,
        '-FileName', `"${fileName}"`,
        '-ActionDescription', `"${actionDescription}"`,
        '-PathDescription', `"${pathDescription}"`,
        '-BuildArguments', `"${buildArgumentsString}"`,
        '-CompilerName', `"${config.name}"`
      ];

      // Show information message
      window.showInformationMessage(`Starting ${actionDescription} for ${fileName} using ${config.name}...`);

      // Get the terminal and show it
      const terminal = this.getCompilerTerminal();
      terminal.show(true);

      // Build the PowerShell command
      const command = `powershell.exe ${psArgs.join(' ')}`;

      // Debug: Log the PowerShell command
      console.log('PowerShell command:', command);

      // Execute the command in the terminal
      terminal.sendText(command);

    } catch (error) {
      window.showErrorMessage(`Failed to ${recreate ? 'recreate' : 'compile'} project: ${error}`);
    }
  }

  /**
   * Get the current compiler configuration from VS Code settings
   */
  private static async getCurrentCompilerConfiguration(): Promise<CompilerConfiguration | null> {
    try {
      const config = workspace.getConfiguration('delphi-utils.compiler');
      const configurations: CompilerConfiguration[] = config.get('configurations', []);
      const currentConfigName: string = config.get('currentConfiguration', 'Delphi 12');

      // Find the current configuration
      const currentConfig = configurations.find(cfg => cfg.name === currentConfigName);

      if (!currentConfig) {
        // If current config not found, use the first available
        if (configurations.length > 0) {
          window.showWarningMessage(`Compiler configuration '${currentConfigName}' not found. Using '${configurations[0].name}' instead.`);
          return configurations[0];
        }
        return null;
      }

      return currentConfig;
    } catch (error) {
      console.error('Failed to get compiler configuration:', error);
      return null;
    }
  }

  /**
   * Get available compiler configurations for UI selection
   */
  static getAvailableConfigurations(): CompilerConfiguration[] {
    const config = workspace.getConfiguration('delphi-utils.compiler');
    return config.get('configurations', []);
  }

  /**
   * Set the current compiler configuration
   */
  static async setCurrentConfiguration(configurationName: string): Promise<void> {
    const config = workspace.getConfiguration('delphi-utils.compiler');
    await config.update('currentConfiguration', configurationName, false);
    window.showInformationMessage(`Compiler configuration set to: ${configurationName}`);
  }

  /**
   * Dispose of the terminal when extension is deactivated
   */
  static dispose(): void {
    if (this.terminal) {
      this.terminal.dispose();
      this.terminal = undefined;
    }
  }
}
