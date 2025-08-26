import { commands, env, Uri, window, Disposable } from "vscode";
import { basename, dirname, join } from "path";
import { promises as fs } from "fs";
import { Runtime } from "../runtime";
import { Projects } from "../constants";
import { Coroutine } from "../typings";
import { GroupProjectEntity, ProjectEntity } from "../db/entities";
import { WorkspaceViewMode } from "../types";
import { DelphiProjectTreeItem } from "./treeItems/delphiProjectTreeItem";
import { DelphiProject } from "./treeItems/delphiProject";
import { ProjectDiscovery } from "./data/projectDiscovery";


export namespace ProjectsCommands {
  export function register() {
    Runtime.extension.subscriptions.push(...[
      ...SelectedProject.registers,
      ...ContextMenu.registers,
      ...ProjectsTreeView.registers
    ]);
  }
  export class SelectedProject {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(Projects.Command.CompileSelectedProject, this.compileSelectedProject.bind(this)),
        commands.registerCommand(Projects.Command.RecreateSelectedProject, this.recreateSelectedProject.bind(this)),
        commands.registerCommand(Projects.Command.RunSelectedProject, this.runSelectedProject.bind(this)),
      ];
    }

    private static async selectedProjectAction(callback: Coroutine<void, [ProjectEntity]>): Promise<void> {
      const workspace = await Runtime.db.getWorkspace();
      if (!workspace?.currentProject) { return; }
      await callback(workspace.currentProject);
    }

    private static async compileSelectedProject() {
      await this.selectedProjectAction(async (project) => {
        const path = project.dprojPath || project.dprPath || project.dpkPath;
        if (!path) { return; }
        Runtime.projects.compiler.compile(Uri.file(path), false);
      });
    }

    private static async recreateSelectedProject() {
      await this.selectedProjectAction(async (project) => {
        const path = project.dprojPath || project.dprPath || project.dpkPath;
        if (!path) { return; }
        Runtime.projects.compiler.compile(Uri.file(path), true);
      });
    }

    private static async runSelectedProject() {
      await this.selectedProjectAction(async (project) => {
        if (!project.exePath) { return; }
        try {
          // Use the system's default application handler to launch the executable
          await env.openExternal(Uri.file(project.exePath));
        } catch (error) {
          window.showErrorMessage(`Failed to launch executable: ${error}`);
        }
      });
    }
  }

  export class ContextMenu {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(Projects.Command.Compile, this.compile.bind(this)),
        commands.registerCommand(Projects.Command.Recreate, this.recreate.bind(this)),
        commands.registerCommand(Projects.Command.ShowInExplorer, this.showInExplorer.bind(this)),
        commands.registerCommand(Projects.Command.OpenInFileExplorer, this.openInFileExplorer.bind(this)),
        commands.registerCommand(Projects.Command.RunExecutable, this.runExecutable.bind(this)),
        commands.registerCommand(Projects.Command.ConfigureOrCreateIni, this.configureOrCreateIni.bind(this)),
        commands.registerCommand(Projects.Command.SelectProject, this.selectProject.bind(this))
      ];
    }
    private static async compile(item: DelphiProjectTreeItem): Promise<void> {
      let file = item.projectDproj || item.projectDpr || item.projectDpk;
      if (file) {
        await Runtime.projects.compiler.compile(file, false);
      }
    }

    private static async recreate(item: DelphiProjectTreeItem): Promise<void> {
      let file = item.projectDproj || item.projectDpr || item.projectDpk;
      if (file) {
        await Runtime.projects.compiler.compile(file, true);
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
      // Try to use default.ini if it exists
      let iniPath = join(
        dirname(item.projectExe!.fsPath),
        `${basename(item.projectExe!.fsPath, ".exe")}.ini`
      );
      let content = `; ${iniPath}\n[CmdLineParam]\n`;
      const defaultIniPath = Runtime.extension.asAbsolutePath("dist/default.ini");
      try {
        content = await fs.readFile(defaultIniPath, "utf8");
      } catch { }

      await fs.writeFile(iniPath, content, "utf8");
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

    private static async selectProject(item: DelphiProjectTreeItem): Promise<void> {
      await Runtime.db.modify(async (ws) => {
        let projects: ProjectEntity[] = [];
        switch (ws.viewMode) {
          case WorkspaceViewMode.GroupProject:
            if (ws.currentGroupProject) {
              projects = ws.currentGroupProject.projects;
            }
            break;
          case WorkspaceViewMode.Discovery:
            projects = ws.discoveredProjects;
            break;
        }
        const project = projects.find((p) => p.id === item.project.projectId);
        if (!project) { return ws; }
        switch (ws.viewMode) {
          case WorkspaceViewMode.GroupProject:
            ws.currentGroupProject!.currentProject = project;
            break;
          case WorkspaceViewMode.Discovery:
            ws.currentProject = project;
            break;
        }
        return ws;
      });
      await Runtime.projects.treeView.refreshTreeView();
    }
  }

  export class Compiler {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(
          Projects.Command.SelectCompilerConfiguration,
          this.selectCompilerConfiguration.bind(this)
        ),
      ];
    }

    public static async selectCompilerConfiguration(): Promise<void> {
      const configurations = Runtime.projects.compiler.availableConfigurations;

      if (!configurations.length) {
        window.showErrorMessage(
          "No compiler configurations found. Please configure Delphi compiler settings."
        );
        return;
      }

      const items = configurations.map((config) => ({
        label: config.name,
        description: config.rsVarsPath,
        detail: `MSBuild: ${config.msBuildPath}`,
      }));

      const selected = await window.showQuickPick(items, {
        placeHolder: "Select Delphi Compiler Configuration",
        matchOnDescription: true,
        matchOnDetail: true,
      });

      Runtime.projects.compiler.configuration = selected?.label;
    }
  }

  export class ProjectsTreeView {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(Projects.Command.Refresh, this.refreshDelphiProjects.bind(this)),
        commands.registerCommand(Projects.Command.PickGroupProject, this.pickGroupProject.bind(this)),
        commands.registerCommand(Projects.Command.UnloadGroupProject, this.unloadGroupProject.bind(this)),
        commands.registerCommand(Projects.Command.EditDefaultIni, this.editDefaultIni.bind(this))
      ];
    }

    private static async refreshDelphiProjects(): Promise<void> {
      if (!await Runtime.assertWorkspaceAvailable()) {
        window.showWarningMessage('No workspace available. Please open a workspace to refresh Delphi projects.');
        return;
      }
      await Runtime.projects.treeView.refreshTreeView(true);
    }

    private static async pickGroupProject(): Promise<void> {
      const uri = await Runtime.projects.treeView.groupProjPicker.pickGroupProject();
      if (!uri) { return; }
      let needToFindProjects = false;

      let ws = await Runtime.db.modify(async (ws) => {
        let groupProj = await Runtime.db.getGroupProject(uri);
        if (groupProj) {
          ws.currentGroupProject = groupProj;
          return ws;
        }
        groupProj = new GroupProjectEntity();
        groupProj.name = basename(uri.fsPath);
        groupProj.path = uri.fsPath;
        needToFindProjects = true;
        ws.currentGroupProject = groupProj;
        return ws;
      });
      if (needToFindProjects) {
        ws = await Runtime.db.modify(async (ws) => {
          ws.currentGroupProject!.projects = await new ProjectDiscovery().findFilesFromGroupProj(ws.currentGroupProject!);
          return ws;
        });
      }
      await Runtime.projects.treeView.refreshTreeView();
      window.showInformationMessage(`Loaded group project: ${ws?.currentGroupProject?.name}`);
    }

    private static async unloadGroupProject(): Promise<void> {
      await Runtime.db.modify(async (ws) => {
        if (ws.viewMode === WorkspaceViewMode.GroupProject) {
          ws.currentGroupProject = null;
          ws.lastUpdated = new Date();
        }
        return ws;
      });
      await Runtime.projects.treeView.refreshTreeView();
      window.showInformationMessage('Unloaded group project. Showing default projects (if discovery is enabled).');
    }

    private static async editDefaultIni(): Promise<void> {
      const defaultIniPath = Runtime.extension.asAbsolutePath("dist/default.ini");
      try {
        await commands.executeCommand("vscode.open", Uri.file(defaultIniPath));
      } catch (error) {
        window.showErrorMessage(`Failed to open default.ini: ${error}`);
      }
    }
  }
}