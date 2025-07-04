import { commands, window, Uri, env, workspace } from 'vscode';
import { DelphiProjectTreeItem } from '../DelphiProjectTreeItem';
import { DelphiProject } from '../DelphiProject';
import { DprFile } from '../DprFile';
import { DprojFile } from '../DprojFile';
import { DpkFile } from '../DpkFile';
import { ExecutableFile } from '../ExecutableFile';
import { IniFile } from '../IniFile';
import { DelphiProjectUtils } from '../utils';
import { Compiler } from './Compiler';
import { basename, dirname, join } from 'path';
import { promises as fs } from 'fs';

/**
 * Context menu commands for Delphi Projects tree items
 */
export class DelphiProjectContextMenuCommands {
  /**
   * Register all context menu commands
   */
  static registerCommands() {
    return [
      // Build commands
      commands.registerCommand('delphi-utils.compileDpr', DelphiProjectContextMenuCommands.compileDpr),
      commands.registerCommand('delphi-utils.recreateDpr', DelphiProjectContextMenuCommands.recreateDpr),

      // File commands
      commands.registerCommand('delphi-utils.showInExplorer', DelphiProjectContextMenuCommands.showInExplorer),
      commands.registerCommand('delphi-utils.openInFileExplorer', DelphiProjectContextMenuCommands.openInFileExplorer),

      // Run commands
      commands.registerCommand('delphi-utils.runExecutable', DelphiProjectContextMenuCommands.runExecutable),

      // INI file commands
      commands.registerCommand('delphi-utils.configureCreateIni', DelphiProjectContextMenuCommands.configureCreateIni),

      // Compiler configuration
      commands.registerCommand('delphi-utils.selectCompilerConfiguration', DelphiProjectContextMenuCommands.selectCompilerConfiguration)
    ];
  }

  /**
   * Compile DPR project - works for any item type by finding the associated DPR/DPROJ
   */
  private static async compileDpr(item: DelphiProjectTreeItem): Promise<void> {
    const dprojUri = await DelphiProjectContextMenuCommands.findDprojUri(item);
    if (dprojUri) {
      await Compiler.compile(dprojUri, false);
    }
  }

  /**
   * Recreate DPR project - works for any item type by finding the associated DPR/DPROJ
   */
  private static async recreateDpr(item: DelphiProjectTreeItem): Promise<void> {
    const dprojUri = await DelphiProjectContextMenuCommands.findDprojUri(item);
    if (dprojUri) {
      await Compiler.compile(dprojUri, true);
    }
  }

  /**
   * Helper method to find the DPROJ URI from any tree item
   */
  private static async findDprojUri(item: DelphiProjectTreeItem): Promise<Uri | null> {
    if (item instanceof DelphiProject) {
      // For DelphiProject, get the main resource URI which should be DPROJ
      return item.getResourceUri();
    } else if (item instanceof DprojFile) {
      return item.resourceUri;
    } else if (item instanceof DprFile) {
      // For DPR files that are children of projects, find the parent DPROJ
      return await DelphiProjectUtils.findDprojFromDpr(item.resourceUri);
    } else if (item instanceof DpkFile) {
      // For DPK files that are children of projects, find the parent DPROJ
      return await DelphiProjectUtils.findDprojFromDpk(item.resourceUri);
    } else if (item instanceof ExecutableFile) {
      // For executable files, find the associated project files
      const projectFiles = await DelphiProjectUtils.findProjectFromExecutable(item.resourceUri);
      if (projectFiles.dproj) {
        return projectFiles.dproj;
      } else {
        window.showWarningMessage(`No DPROJ file found for executable: ${item.label}`);
        return null;
      }
    } else if (item instanceof IniFile) {
      // For INI files, find the associated project through the executable
      const executableName = basename(item.resourceUri.fsPath).replace('.ini', '.exe');
      const executablePath = join(dirname(item.resourceUri.fsPath), executableName);
      const executableUri = Uri.file(executablePath);

      const projectFiles = await DelphiProjectUtils.findProjectFromExecutable(executableUri);
      if (projectFiles.dproj) {
        return projectFiles.dproj;
      } else {
        window.showWarningMessage(`No DPROJ file found for INI file: ${item.label}`);
        return null;
      }
    } else {
      window.showWarningMessage(`Cannot compile unknown file type: ${item.label}`);
      return null;
    }
  }

  /**
   * Select compiler configuration
   */
  private static async selectCompilerConfiguration(): Promise<void> {
    const configurations = Compiler.getAvailableConfigurations();

    if (configurations.length === 0) {
      window.showErrorMessage('No compiler configurations found. Please configure Delphi compiler settings.');
      return;
    }

    const items = configurations.map(config => ({
      label: config.name,
      description: config.rsVarsPath,
      detail: `MSBuild: ${config.msBuildPath}`
    }));

    const selected = await window.showQuickPick(items, {
      placeHolder: 'Select Delphi Compiler Configuration',
      matchOnDescription: true,
      matchOnDetail: true
    });

    if (selected) {
      await Compiler.setCurrentConfiguration(selected.label);
    }
  }

  /**
   * Show file in VS Code explorer
   */
  private static async showInExplorer(item: DelphiProjectTreeItem): Promise<void> {
    try {
      // Focus the file in VS Code explorer
      await commands.executeCommand('revealInExplorer', item.resourceUri);
    } catch (error) {
      window.showErrorMessage(`Failed to show in explorer: ${error}`);
    }
  }

  /**
   * Open containing folder in system file explorer
   */
  private static async openInFileExplorer(item: DelphiProjectTreeItem): Promise<void> {
    try {
      // Open the containing folder in system file explorer
      const folderUri = Uri.file(item.resourceUri.fsPath.substring(0, item.resourceUri.fsPath.lastIndexOf('\\')));
      await env.openExternal(folderUri);
    } catch (error) {
      window.showErrorMessage(`Failed to open in file explorer: ${error}`);
    }
  }

  /**
   * Run executable file - works for any item type by finding the associated executable
   */
  private static async runExecutable(item: DelphiProjectTreeItem): Promise<void> {
    try {
      let executableUri: Uri | undefined;

      if (item instanceof DelphiProject) {
        // Delphi project - use the executable if available
        executableUri = item.executable;
      } else if (item instanceof ExecutableFile) {
        // Direct executable file
        executableUri = item.resourceUri;
      } else if (item instanceof DprojFile) {
        // DPROJ file - find the executable
        const foundExecutable = await DelphiProjectUtils.findExecutableFromDproj(item.resourceUri);
        executableUri = foundExecutable || undefined;
      } else if (item instanceof DprFile || item instanceof DpkFile) {
        // Find the parent DPROJ and then the executable
        const dprojUri = await DelphiProjectContextMenuCommands.findDprojUri(item);
        if (dprojUri) {
          const foundExecutable = await DelphiProjectUtils.findExecutableFromDproj(dprojUri);
          executableUri = foundExecutable || undefined;
        }
      } else if (item instanceof IniFile) {
        // INI file - find the corresponding executable
        const executableName = basename(item.resourceUri.fsPath).replace('.ini', '.exe');
        const executablePath = join(dirname(item.resourceUri.fsPath), executableName);
        executableUri = Uri.file(executablePath);
      }

      if (executableUri) {
        await env.openExternal(executableUri);
        window.showInformationMessage(`Running: ${executableUri.fsPath}`);
      } else {
        window.showWarningMessage(`No executable found for: ${item.label}`);
      }
    } catch (error) {
      window.showErrorMessage(`Failed to run executable: ${error}`);
    }
  }
  /**
   * Configure/Create INI file for an executable
   */
  private static async configureCreateIni(item: DelphiProjectTreeItem): Promise<void> {
    try {
      let executableUri: Uri | undefined;

      if (item instanceof DelphiProject) {
        // Delphi project - use the executable if available
        executableUri = item.executable;
      } else if (item instanceof ExecutableFile) {
        // Direct executable file
        executableUri = item.resourceUri;
      } else if (item instanceof IniFile) {
        // INI file - find the corresponding executable
        const executableName = basename(item.resourceUri.fsPath).replace('.ini', '.exe');
        const executablePath = join(dirname(item.resourceUri.fsPath), executableName);
        executableUri = Uri.file(executablePath);
      } else if (item instanceof DprojFile) {
        // DPROJ file - find the executable
        const foundExecutable = await DelphiProjectUtils.findExecutableFromDproj(item.resourceUri);
        executableUri = foundExecutable || undefined;
      } else if (item instanceof DprFile || item instanceof DpkFile) {
        // Find the parent DPROJ and then the executable
        const dprojUri = await DelphiProjectContextMenuCommands.findDprojUri(item);
        if (dprojUri) {
          const foundExecutable = await DelphiProjectUtils.findExecutableFromDproj(dprojUri);
          executableUri = foundExecutable || undefined;
        }
      }

      if (!executableUri) {
        window.showWarningMessage(`No executable found for: ${item.label}`);
        return;
      }

      // Calculate INI file path
      const executableDir = dirname(executableUri.fsPath);
      const executableName = basename(executableUri.fsPath).replace(/\.[^/.]+$/, "");
      const iniPath = join(executableDir, `${executableName}.ini`);
      const iniUri = Uri.file(iniPath);

      // Check if INI file already exists
      try {
        await fs.access(iniPath);
        // File exists, open it for editing
        await commands.executeCommand('vscode.open', iniUri);
        window.showInformationMessage(`Opened existing INI file: ${iniPath}`);
      } catch {
        // File doesn't exist, create it
        const defaultIniContent = `; Configuration file for ${executableName}
; Created by Delphi Utils VS Code Extension

[Settings]
; Add your application settings here

[Options]
; Add your application options here
`;

        try {
          await fs.writeFile(iniPath, defaultIniContent, 'utf8');
          await commands.executeCommand('vscode.open', iniUri);
          window.showInformationMessage(`Created and opened new INI file: ${iniPath}`);

          // Refresh the explorer to show the new INI file
          commands.executeCommand('delphi-utils.refreshDelphiProjects');
        } catch (error) {
          window.showErrorMessage(`Failed to create INI file: ${error}`);
        }
      }
    } catch (error) {
      window.showErrorMessage(`Failed to configure INI file: ${error}`);
    }
  }
}
