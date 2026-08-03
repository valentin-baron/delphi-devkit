import { commands, env, Uri, window, Disposable, workspace } from "vscode";
import { COMMANDS, FORMAT } from "./constants";
import { join } from "path";
import { promises as fs } from 'fs';
import { Runtime } from "./runtime";
import { env as osEnv } from "process";
import { pickAndSelectProject } from "./projects/pickers/projectPicker";

export class GeneralCommands {
  public static get registers(): Disposable[] {
    return [
      commands.registerCommand(COMMANDS.EXPORT_PROJECTS, this.exportProjects.bind(this)),
      commands.registerCommand(COMMANDS.IMPORT_PROJECTS, this.importProjects.bind(this)),
      commands.registerCommand(COMMANDS.EXPORT_COMPILERS, this.exportCompilers.bind(this)),
      commands.registerCommand(COMMANDS.IMPORT_COMPILERS, this.importCompilers.bind(this)),
      commands.registerCommand(COMMANDS.EDIT_COMPILER_CONFIGURATIONS, this.editCompilerConfigurations.bind(this)),
      commands.registerCommand(COMMANDS.RESET_COMPILER_CONFIGURATIONS, this.resetCompilerConfigurations.bind(this)),
      commands.registerCommand(COMMANDS.EDIT_PROJECTS_DATA, this.editProjectsData.bind(this)),
      commands.registerCommand(COMMANDS.QUICK_PICK_PROJECT, () => pickAndSelectProject()),
      commands.registerCommand(COMMANDS.DUMP_AST_YAML, this.dumpAstYaml.bind(this))
    ];
  }

  // ─── AST YAML dump ─────────────────────────────────────────────────────

  /**
   * Dump the AST of the current unit as YAML and open it. Sends the active
   * editor's document URI to the server via the `ddk/dumpAstYaml` request; on
   * success opens the written `.ast.yaml` file, on failure shows the error.
   */
  private static async dumpAstYaml(): Promise<void> {
    const editor = window.activeTextEditor;
    if (!editor) {
      window.showInformationMessage('Open a Delphi unit (.pas) to dump its AST.');
      return;
    }
    const document = editor.document;
    if (!document.fileName.toLowerCase().endsWith('.pas')) {
      window.showInformationMessage('The active file is not a Delphi unit (.pas).');
      return;
    }
    try {
      const { path } = await Runtime.client.dumpAstYaml(document.uri.toString());
      const opened = await workspace.openTextDocument(Uri.file(path));
      await window.showTextDocument(opened);
    } catch (error) {
      window.showErrorMessage(`Failed to dump AST as YAML: ${error}`);
    }
  }

  private static get ddkDir(): string {
    return join(osEnv.APPDATA || osEnv.HOME || '', 'ddk');
  }

  // ─── Export / Import Projects ──────────────────────────────────────────

  private static async exportProjects(): Promise<void> {
    const sourcePath = join(this.ddkDir, 'projects.ron');
    try {
      await fs.access(sourcePath);
    } catch {
      window.showErrorMessage('No projects.ron file found to export.');
      return;
    }
    const fileUri = await window.showSaveDialog({
      saveLabel: 'Export Projects',
      title: 'Export DDK Projects',
      filters: { 'RON files': ['ron'], 'All files': ['*'] },
      defaultUri: Uri.file(join(env.appRoot, 'projects.ron'))
    });
    if (!fileUri) return;
    try {
      await fs.copyFile(sourcePath, fileUri.fsPath);
      window.showInformationMessage('Projects exported successfully.');
    } catch (error) {
      window.showErrorMessage(`Failed to export projects: ${error}`);
    }
  }

  private static async importProjects(): Promise<void> {
    const fileUri = (
      await window.showOpenDialog({
        canSelectMany: false,
        title: 'Import DDK Projects',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Import',
        filters: { 'RON files': ['ron'], 'All files': ['*'] }
      })
    )?.[0];
    if (!fileUri) return;
    const destPath = join(this.ddkDir, 'projects.ron');
    try {
      await fs.copyFile(fileUri.fsPath, destPath);
      await Runtime.client.refresh();
      await Runtime.projects.workspacesTreeView.refresh();
      await Runtime.projects.groupProjectTreeView.refresh();
      await Runtime.projects.compilerStatusBarItem.updateDisplay();
      window.showInformationMessage('Projects imported successfully.');
    } catch (error) {
      window.showErrorMessage(`Failed to import projects: ${error}`);
    }
  }

  // ─── Export / Import Compilers ─────────────────────────────────────────

  private static async exportCompilers(): Promise<void> {
    const sourcePath = join(this.ddkDir, 'compilers.ron');
    try {
      await fs.access(sourcePath);
    } catch {
      window.showErrorMessage('No compilers.ron file found to export.');
      return;
    }
    const fileUri = await window.showSaveDialog({
      saveLabel: 'Export Compilers',
      title: 'Export DDK Compiler Configurations',
      filters: { 'RON files': ['ron'], 'All files': ['*'] },
      defaultUri: Uri.file(join(env.appRoot, 'compilers.ron'))
    });
    if (!fileUri) return;
    try {
      await fs.copyFile(sourcePath, fileUri.fsPath);
      window.showInformationMessage('Compiler configurations exported successfully.');
    } catch (error) {
      window.showErrorMessage(`Failed to export compiler configurations: ${error}`);
    }
  }

  private static async importCompilers(): Promise<void> {
    const fileUri = (
      await window.showOpenDialog({
        canSelectMany: false,
        title: 'Import DDK Compiler Configurations',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Import',
        filters: { 'RON files': ['ron'], 'All files': ['*'] }
      })
    )?.[0];
    if (!fileUri) return;
    const destPath = join(this.ddkDir, 'compilers.ron');
    try {
      await fs.copyFile(fileUri.fsPath, destPath);
      await Runtime.client.refresh();
      await Runtime.projects.workspacesTreeView.refresh();
      await Runtime.projects.groupProjectTreeView.refresh();
      await Runtime.projects.compilerStatusBarItem.updateDisplay();
      window.showInformationMessage('Compiler configurations imported successfully.');
    } catch (error) {
      window.showErrorMessage(`Failed to import compiler configurations: ${error}`);
    }
  }

  // ─── Direct file editing ──────────────────────────────────────────────

  private static async editCompilerConfigurations(): Promise<void> {
    const path = join(osEnv.APPDATA || osEnv.HOME || '', 'ddk', 'compilers.ron');
    const document = await workspace.openTextDocument(Uri.file(path));
    await window.showTextDocument(document);
    await Runtime.client.refresh();
    await Runtime.projects.workspacesTreeView.refresh();
    await Runtime.projects.groupProjectTreeView.refresh();
    await Runtime.projects.compilerStatusBarItem.updateDisplay();
  }

  private static async resetCompilerConfigurations(): Promise<void> {
    window.showInformationMessage("Not yet implemented.");
  }

  private static async editProjectsData(): Promise<void> {
    const path = join(osEnv.APPDATA || osEnv.HOME || '', 'ddk', 'projects.ron');
    const document = await workspace.openTextDocument(Uri.file(path));
    await window.showTextDocument(document);
    await Runtime.client.refresh();
    await Runtime.projects.workspacesTreeView.refresh();
    await Runtime.projects.groupProjectTreeView.refresh();
    await Runtime.projects.compilerStatusBarItem.updateDisplay();
  }
}

export class FormatterCommands {
    public static get registers(): Disposable[] {
        return [
            commands.registerCommand(FORMAT.COMMAND.EDIT_FORMATTER_CONFIG, this.editConfig.bind(this)),
            commands.registerCommand(FORMAT.COMMAND.RESET_FORMATTER_CONFIG, this.resetConfig.bind(this))
        ];
    }

    private static async editConfig(): Promise<void> {
        const path = join(osEnv.APPDATA || osEnv.HOME || '', 'ddk', 'ddk_formatter.config');
        const document = await workspace.openTextDocument(Uri.file(path));
        await window.showTextDocument(document);
        await Runtime.client.refresh();
        await Runtime.projects.workspacesTreeView.refresh();
        await Runtime.projects.groupProjectTreeView.refresh();
        await Runtime.projects.compilerStatusBarItem.updateDisplay();
    }

    private static async resetConfig(): Promise<void> {
        window.showInformationMessage("Not yet implemented.");
    }
}