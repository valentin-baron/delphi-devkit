import { commands, env, Uri, window, Disposable, workspace, ConfigurationTarget, TreeItem } from "vscode";
import { dirname, join } from "path";
import { promises as fs } from "fs";
import { Runtime } from "../runtime";
import { PROJECTS } from "../constants";
import { Coroutine } from "../typings";
import { Entities } from "../db/entities";
import { DelphiProjectTreeItem } from "./treeItems/delphiProjectTreeItem";
import { DelphiProject } from "./treeItems/delphiProject";
import { ProjectFileDiscovery } from "./data/projectDiscovery";
import { assertError, basenameNoExt } from "../utils";
import { ExtensionDataExport } from "../types";
import { WorkspaceItem } from "./treeItems/workspaceItem";
import { LexoSorter } from "../utils/lexoSorter";


export namespace ProjectsCommands {
  export function register() {
    Runtime.extension.subscriptions.push(...[
      ...SelectedProject.registers,
      ...ContextMenu.registers,
      ...Compiler.registers,
      ...ProjectsTreeView.registers,
      ...Configuration.registers,
    ]);
  }
  export class SelectedProject {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.COMPILE_SELECTED_PROJECT, this.compileSelectedProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE_SELECTED_PROJECT, this.recreateSelectedProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RUN_SELECTED_PROJECT, this.runSelectedProject.bind(this)),
      ];
    }

    private static async selectedProjectAction(callback: Coroutine<void, [DelphiProject]>): Promise<void> {
      const config = await Runtime.db.getConfiguration();
      if (!config.selectedProject) { return; }
      const project = config.selectedProject;
      const item = Runtime.projects.workspacesTreeView.items.find(i => i.entity.id === project.id) ||
        Runtime.projects.groupProjectsTreeView.items.find(i => i.entity.id === project.id);
      if (!item) { return; }
      await callback(item);
    }

    private static async compileSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        if (item.link.owner instanceof Entities.Workspace) {
          Runtime.projects.compiler.compileWorkspaceItem(item, false);
        } else if (item.link.owner instanceof Entities.GroupProject) {
          Runtime.projects.compiler.compileGroupProjectItem(item, false);
        }
      });
    }

    private static async recreateSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        if (item.link.owner instanceof Entities.Workspace) {
          Runtime.projects.compiler.compileWorkspaceItem(item, true);
        } else if (item.link.owner instanceof Entities.GroupProject) {
          Runtime.projects.compiler.compileGroupProjectItem(item, true);
        }
      });
    }

    private static async runSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        if (!item.projectExe) { return; }
        try {
          // Use the system's default application handler to launch the executable
          await env.openExternal(item.projectExe);
        } catch (error) {
          window.showErrorMessage(`Failed to launch executable: ${error}`);
        }
      });
    }
  }

  export class ContextMenu {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.COMPILE, this.compile.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE, this.recreate.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.SHOW_IN_EXPLORER, this.showInExplorer.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.OPEN_IN_FILE_EXPLORER, this.openInFileExplorer.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RUN_EXECUTABLE, this.runExecutable.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.CONFIGURE_OR_CREATE_INI, this.configureOrCreateIni.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.SELECT_PROJECT, this.selectProject.bind(this))
      ];
    }

    private static async compile(item: DelphiProjectTreeItem): Promise<void> {
      if (item.project.link.owner instanceof Entities.Workspace) {
        Runtime.projects.compiler.compileWorkspaceItem(item.project as DelphiProject, false);
      } else if (item.project.link.owner instanceof Entities.GroupProject) {
        Runtime.projects.compiler.compileGroupProjectItem(item.project as DelphiProject, false);
      }
    }

    private static async recreate(item: DelphiProjectTreeItem): Promise<void> {
      if (item.project.link.owner instanceof Entities.Workspace) {
        Runtime.projects.compiler.compileWorkspaceItem(item.project as DelphiProject, true);
      } else if (item.project.link.owner instanceof Entities.GroupProject) {
        Runtime.projects.compiler.compileGroupProjectItem(item.project as DelphiProject, true);
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
        `${basenameNoExt(item.projectExe!.fsPath)}.ini`
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
      await project.setIni(iniPath);
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
      const config = await Runtime.db.getConfiguration();
      config.selectedProject = item.project.entity;
      await Runtime.db.saveConfiguration(config);
    }
  }

  export class Compiler {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(
          PROJECTS.COMMAND.SELECT_COMPILER,
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

      if (!selected) { return; }

      const config = await Runtime.db.getConfiguration();
      config.groupProjectsCompiler = selected.label;
      await Runtime.db.saveConfiguration(config);
      await Runtime.projects.compilerStatusBarItem.updateDisplay();
      window.showInformationMessage(
        `Compiler configuration set to: ${selected?.label}`
      );
    }
  }

  export class ProjectsTreeView {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.REFRESH, this.refreshDelphiProjects.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.SELECT_GROUP_PROJECT, this.pickGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.UNLOAD_GROUP_PROJECT, this.unloadGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.EDIT_DEFAULT_INI, this.editDefaultIni.bind(this))
      ];
    }

    private static async refreshDelphiProjects(): Promise<void> {
      Runtime.projects.workspacesTreeView.refreshTreeView();
      Runtime.projects.groupProjectsTreeView.refreshTreeView();
    }

    private static async pickGroupProject(): Promise<void> {
      const uri = await Runtime.projects.groupProjectPicker.pickGroupProject();
      if (!uri) { return; }
      let groupProject = await Runtime.db.getGroupProject(uri.fsPath);
      if (!groupProject) {
        const projects = await new ProjectFileDiscovery().findFilesFromGroupProj(uri);
        groupProject = await Runtime.db.addGroupProject(basenameNoExt(uri), uri.fsPath, projects);
      }
      const config = await Runtime.db.getConfiguration();
      config.selectedGroupProject = groupProject;
      await Runtime.db.saveConfiguration(config);
      await Runtime.projects.groupProjectsTreeView.refreshTreeView();
      window.showInformationMessage(`Loaded group project: ${groupProject?.name || uri.fsPath}`);
    }

    private static async unloadGroupProject(): Promise<void> {
      const config = await Runtime.db.getConfiguration();
      config.selectedGroupProject = null;
      await Runtime.db.saveConfiguration(config);
      await Runtime.projects.workspacesTreeView.refreshTreeView();
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

  export class Configuration {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.EXPORT_CONFIGURATION, this.exportConfiguration.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.IMPORT_CONFIGURATION, this.importConfiguration.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.ADD_PROJECT, this.addProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.REMOVE_PROJECT, this.removeProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.ADD_WORKSPACE, this.addWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RENAME_WORKSPACE, this.renameWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.REMOVE_WORKSPACE, this.removeWorkspace.bind(this)),
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
      if (!fileUri) { return; }
      try {
        const config = await Runtime.db.getConfiguration();
        const data = new ExtensionDataExport.FileContent(
          config,
          Runtime.projects.compiler.availableConfigurations
        );
        await fs.writeFile(fileUri.fsPath, JSON.stringify(data, null, 2), 'utf8');
        window.showInformationMessage('Configuration exported successfully.');
      } catch (error) {
        window.showErrorMessage(`Failed to export configuration: ${error}`);
      }
    }

    private static async importConfigurationV1_0(data: ExtensionDataExport.FileContent): Promise<void> {
      await Runtime.db.saveConfiguration(data.configuration);
      if (data.compilers) {
        await workspace.getConfiguration(PROJECTS.CONFIG.KEY).update(
          PROJECTS.CONFIG.COMPILER.CONFIGURATIONS,
          data.compilers || [],
          ConfigurationTarget.Global
        );
      }
    }

    private static async importConfiguration(): Promise<void> {
      const fileUri = (await window.showOpenDialog({
        canSelectMany: false,
        title: 'Import DDK Configuration',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Import',
        filters: {
          'DDK JSON files': ['ddk.json'],
          'All files': ['*']
        }
      }))?.[0];
      if (!fileUri) { return; }
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
          await Runtime.projects.workspacesTreeView.refreshTreeView();
          await Runtime.projects.groupProjectsTreeView.refreshTreeView();
          await Runtime.projects.compilerStatusBarItem.updateDisplay();
          window.showInformationMessage('Configuration imported successfully.');
        } else {
          window.showErrorMessage('Invalid configuration file.');
        }
      } catch (error) {
        window.showErrorMessage(`Failed to import configuration: ${error}`);
      }
    }

    private static async addProject(
      item: TreeItem
    ): Promise<void> {
      const itemInWorkspaceContext = (i: TreeItem) => {
        return (i instanceof WorkspaceItem) || (i instanceof DelphiProjectTreeItem && i.project.link.owner instanceof Entities.Workspace);
      };
      if (!assertError(itemInWorkspaceContext(item), 'This command only works when invoked inside the context of a workspace tree item.')) {
        return;
      }
      const ws = item instanceof WorkspaceItem ?
        item.workspace :
        (item as DelphiProjectTreeItem).project.link.owner as Entities.Workspace;

      if (!assertError(ws, 'Selected workspace not found.')) {
        return;
      }
      const uris = (await window.showOpenDialog({
        canSelectMany: true,
        title: 'Add Delphi Projects',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Add',
        filters: {
          'Delphi Project Files': ['dproj', 'dpr', 'dpk']
        }
      }));
      if (!assertError(uris, 'No project files selected')) { return; }
      await Promise.all(
        uris!.map(async (uri) => {
          if (!uri) { return; }
          const projectName = basenameNoExt(uri);
          let files = await new ProjectFileDiscovery().findFiles(Uri.file(dirname(uri.fsPath)), basenameNoExt(uri));
          if (files.isEmpty) { return; }
          const project = new Entities.Project();
          project.name = projectName;
          project.path = join(dirname(uri.fsPath), projectName);
          project.dproj = files.dproj?.fsPath || null;
          project.dpr = files.dpr?.fsPath || null;
          project.dpk = files.dpk?.fsPath || null;
          project.exe = files.exe?.fsPath || null;
          project.ini = files.ini?.fsPath || null;
          const link = new Entities.WorkspaceProjectLink();
          link.project = project;
          link.workspace = ws;
          ws.projects.push(link);
        })
      );
      ws.projects = new LexoSorter(ws.projects).items;
      await Runtime.db.saveWorkspace(ws);
      Runtime.projects.workspacesTreeView.refreshTreeView();
    }

    private static async removeProject(
      item: DelphiProjectTreeItem
    ): Promise<void> {
      const project = item.project.entity;
      const link = item.project.link;
      if (!assertError(link.owner instanceof Entities.Workspace, 'Only workspace projects can be removed.')) {
        return;
      }
      const confirm = await window.showWarningMessage(
        `Are you sure you want to remove project ${project.name} from workspace "${link.owner.name}"? This will not delete any files.`,
        { modal: true },
        'Remove'
      );
      if (confirm !== 'Remove') { return; }
      await Runtime.db.removeProjectLink(link);
      await Runtime.projects.workspacesTreeView.refreshTreeView();
      window.showInformationMessage(`Removed project: ${item.label}`);
    }

    private static checkWorkspaceName(name: string, config: Entities.Configuration): string | undefined {
      if (!name || !name.trim()) {
        return 'Workspace name cannot be empty';
      } else if (config.workspaces.some(ws => ws.name.toLowerCase() === name.trim().toLowerCase())) {
        return 'A workspace with this name already exists';
      }
    }

    private static async addWorkspace(): Promise<void> {
      const config = await Runtime.db.getConfiguration();
      const name = await window.showInputBox({
        prompt: 'Enter a name for the new workspace',
        placeHolder: 'Workspace Name',
        validateInput: (value) => this.checkWorkspaceName(value, config)
      });
      if (!assertError(name, 'Cannot create Workspace without name.')) { return; }
      const compilerName = await window.showQuickPick(
        Runtime.projects.compiler.availableConfigurations.map(cfg => ({
          label: cfg.name,
          description: cfg.rsVarsPath,
          detail: `MSBuild: ${cfg.msBuildPath}`
        })),
        {
          placeHolder: 'Select Delphi Compiler Configuration for this workspace',
          matchOnDescription: false,
          matchOnDetail: false,
          canPickMany: false
        }
      );
      if (!assertError(compilerName, 'Cannot create Workspace without compiler configuration.')) { return; }
      const workspace = await Runtime.db.addWorkspace(name!.trim(), compilerName!.label);
      await Runtime.projects.workspacesTreeView.refreshTreeView();
      window.showInformationMessage(`Created new workspace: ${workspace.name}`);
    }

    private static async removeWorkspace(
      item: WorkspaceItem
    ): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) {
        return;
      }
      const ws = item.workspace;
      await Runtime.db.removeWorkspace(ws);
      await Runtime.projects.workspacesTreeView.refreshTreeView();
      window.showInformationMessage(`Removed workspace: ${ws.name}`);
    }

    private static async renameWorkspace(
      item: WorkspaceItem
    ): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) {
        return;
      }
      const ws = item.workspace;
      const config = await Runtime.db.getConfiguration();
      const newName = await window.showInputBox({
        prompt: 'Enter a new name for the workspace',
        placeHolder: 'Workspace Name',
        value: ws.name,
        validateInput: (value) => this.checkWorkspaceName(value, config)
      });
      if (!assertError(newName && newName.trim() && newName.trim() !== ws.name, 'Workspace name not changed.')) { return; }
      ws.name = newName!.trim();
      await Runtime.db.saveWorkspace(ws);
      await Runtime.projects.workspacesTreeView.refreshTreeView();
      window.showInformationMessage(`Renamed workspace to: ${ws.name}`);
    }
  }
}