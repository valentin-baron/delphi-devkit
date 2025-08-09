import { commands, window, Uri, env, workspace } from "vscode";
import { DelphiProjectTreeItem } from "../treeItems/delphiProjectTreeItem";
import { DelphiProject } from "../treeItems/delphiProject";
import { basename, dirname, join } from "path";
import { promises as fs } from "fs";
import { Runtime } from "../../runtime";
import { Projects } from "../../constants";

/**
 * Context menu commands for Delphi Projects tree items
 */
export class DelphiProjectContextMenuCommands {
  /**
   * Register all context menu commands
   */
  static registerCommands() {
    return [
      commands.registerCommand(
        Projects.Command.Compile,
        DelphiProjectContextMenuCommands.compile
      ),
      commands.registerCommand(
        Projects.Command.Recreate,
        DelphiProjectContextMenuCommands.recreate
      ),
      commands.registerCommand(
        Projects.Command.ShowInExplorer,
        DelphiProjectContextMenuCommands.showInExplorer
      ),
      commands.registerCommand(
        Projects.Command.OpenInFileExplorer,
        DelphiProjectContextMenuCommands.openInFileExplorer
      ),
      commands.registerCommand(
        Projects.Command.RunExecutable,
        DelphiProjectContextMenuCommands.runExecutable
      ),
      commands.registerCommand(
        Projects.Command.ConfigureOrCreateIni,
        DelphiProjectContextMenuCommands.configureOrCreateIni
      ),
    ];
  }

  private static async compile(item: DelphiProjectTreeItem): Promise<void> {
    let file = item.projectDproj || item.projectDpr || item.projectDpk;
    if (file) {
      await Runtime.compiler.compile(file, false);
    }
  }

  private static async recreate(item: DelphiProjectTreeItem): Promise<void> {
    let file = item.projectDproj || item.projectDpr || item.projectDpk;
    if (file) {
      await Runtime.compiler.compile(file, true);
    }
  }

  private static async showInExplorer(
    item: DelphiProjectTreeItem
  ): Promise<void> {
    try {
      // Focus the file in VS Code explorer
      await commands.executeCommand("revealInExplorer", item.resourceUri);
    } catch (error) {
      window.showErrorMessage(`Failed to show in explorer: ${error}`);
    }
  }

  private static async openInFileExplorer(
    item: DelphiProjectTreeItem
  ): Promise<void> {
    try {
      // Open the containing folder in system file explorer
      const folderUri = Uri.file(dirname(item.resourceUri.fsPath));
      await env.openExternal(folderUri);
    } catch (error) {
      window.showErrorMessage(`Failed to open in file explorer: ${error}`);
    }
  }

  private static async runExecutable(
    item: DelphiProjectTreeItem
  ): Promise<void> {
    if (item.projectExe) {
      await env.openExternal(item.projectExe);
      window.showInformationMessage(`Running: ${item.projectExe.fsPath}`);
    } else {
      window.showWarningMessage(`No executable found for: ${item.label}`);
    }
  }

  private static async createIniFile(
    item: DelphiProjectTreeItem
  ): Promise<void> {
    // File doesn't exist, create it
    // Try to use .vscode/.delphi/default.ini if it exists
    const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
    let iniPath = join(
      dirname(item.projectExe!.fsPath),
      `${basename(item.projectExe!.fsPath, ".exe")}.ini`
    );
    let defaultIniContent = `; ${iniPath}\n[CmdLineParam]\n`;
    let usedDefault = false;
    if (workspaceRoot) {
      const defaultIniPath = join(
        workspaceRoot,
        ".vscode",
        ".delphi",
        "default.ini"
      );
      try {
        const content = await fs.readFile(defaultIniPath, "utf8");
        defaultIniContent = content;
        usedDefault = true;
      } catch {}
    }

    await fs.writeFile(iniPath, defaultIniContent, "utf8");
    await commands.executeCommand("vscode.open", iniPath);
    window.showInformationMessage(
      `Created and opened new INI file: ${iniPath}`
    );

    let project = item.project ? item.project : item;
    if (!(project instanceof DelphiProject)) {
      return;
    }
    await project.setIni(Uri.file(iniPath), true);
  }

  private static async configureOrCreateIni(
    item: DelphiProjectTreeItem
  ): Promise<void> {
    if (!item.projectExe) {
      window.showWarningMessage(
        `No executable for: ${item.label} - cannot create INI file.`
      );
      return;
    }
    if (item.projectIni) {
      try {
        await fs.access(item.projectIni.fsPath);
        // File exists, open it for editing
        await commands.executeCommand("vscode.open", item.projectIni);
        window.showInformationMessage(
          `Opened existing INI file: ${item.projectIni.fsPath}`
        );
        return;
      } catch {
        // File doesn't exist, fall through to create it
      }
    }
    await this.createIniFile(item);
  }
}
