import { commands, env, Uri, window, Disposable, TreeItem } from 'vscode';
import { dirname, join } from 'path';
import { promises as fs } from 'fs';
import { Runtime } from '../runtime';
import { PROJECTS } from '../constants';
import { Coroutine } from '../typings';
import { Entities } from '../db/entities';
import { BaseFileItem } from './trees/items/baseFile';
import { ProjectItem } from './trees/items/project';
import { ProjectFileDiscovery } from './data/projectDiscovery';
import { assertError, basenameNoExt } from '../utils';
import { ProjectLinkType } from '../types';
import { WorkspaceItem } from './trees/items/workspaceItem';
import { LexoSorter } from '../utils/lexoSorter';

export namespace ProjectsCommands {
  export function register() {
    Runtime.extension.subscriptions.push(
      ...[...SelectedProject.registers, ...ContextMenu.registers, ...Compiler.registers, ...ProjectsTreeView.registers, ...Configuration.registers]
    );
  }
  export class SelectedProject {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.COMPILE_SELECTED_PROJECT, this.compileSelectedProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE_SELECTED_PROJECT, this.recreateSelectedProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RUN_SELECTED_PROJECT, this.runSelectedProject.bind(this))
      ];
    }

    private static async selectedProjectAction(callback: Coroutine<void, [ProjectItem]>): Promise<void> {
      const config = Runtime.configEntity;
      if (!config.selectedProject) return;
      const project = config.selectedProject;
      const item =
        Runtime.projects.workspacesTreeView.projects.find((i) => i.entity.id === project.id) ||
        Runtime.projects.groupProjectTreeView.projects.find((i) => i.entity.id === project.id);
      if (!item) return;
      await callback(item);
    }

    private static async compileSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        switch (item.link.linkType) {
          case ProjectLinkType.Workspace:
            Runtime.projects.compiler.compileWorkspaceItem(item, false);
            break;
          case ProjectLinkType.GroupProject:
            Runtime.projects.compiler.compileGroupProjectItem(item, false);
            break;
        }
      });
    }

    private static async recreateSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        switch (item.link.linkType) {
          case ProjectLinkType.Workspace:
            Runtime.projects.compiler.compileWorkspaceItem(item, true);
            break;
          case ProjectLinkType.GroupProject:
            Runtime.projects.compiler.compileGroupProjectItem(item, true);
            break;
        }
      });
    }

    private static async runSelectedProject() {
      await this.selectedProjectAction(async (item) => {
        if (!item.projectExe) return;
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

    private static async compile(item: BaseFileItem): Promise<void> {
      switch (item.project.link.linkType) {
        case ProjectLinkType.Workspace:
          Runtime.projects.compiler.compileWorkspaceItem(item.project as ProjectItem, false);
          break;
        case ProjectLinkType.GroupProject:
          Runtime.projects.compiler.compileGroupProjectItem(item.project as ProjectItem, false);
          break;
      }
    }

    private static async recreate(item: BaseFileItem): Promise<void> {
      switch (item.project.link.linkType) {
        case ProjectLinkType.Workspace:
          Runtime.projects.compiler.compileWorkspaceItem(item.project as ProjectItem, true);
          break;
        case ProjectLinkType.GroupProject:
          Runtime.projects.compiler.compileGroupProjectItem(item.project as ProjectItem, true);
          break;
      }
    }

    private static async showInExplorer(item: BaseFileItem): Promise<void> {
      try {
        // Focus the file in VS Code explorer
        await commands.executeCommand('revealInExplorer', item.resourceUri);
      } catch (error) {
        window.showErrorMessage(`Failed to show in explorer: ${error}`);
      }
    }

    private static async openInFileExplorer(item: BaseFileItem): Promise<void> {
      try {
        // Open the containing folder in system file explorer
        const folderUri = Uri.file(dirname(item.resourceUri.fsPath));
        await env.openExternal(folderUri);
      } catch (error) {
        window.showErrorMessage(`Failed to open in file explorer: ${error}`);
      }
    }

    private static async runExecutable(item: BaseFileItem): Promise<void> {
      if (item.projectExe) {
        await env.openExternal(item.projectExe);
        window.showInformationMessage(`Running: ${item.projectExe.fsPath}`);
      } else window.showWarningMessage(`No executable found for: ${item.label}`);
    }

    private static async createIniFile(item: BaseFileItem): Promise<void> {
      // File doesn't exist, create it
      // Try to use default.ini if it exists
      let iniPath = join(dirname(item.projectExe!.fsPath), `${basenameNoExt(item.projectExe!.fsPath)}.ini`);
      let content = `; ${iniPath}\n[CmdLineParam]\n`;
      const defaultIniPath = Runtime.extension.asAbsolutePath('dist/default.ini');
      try {
        content = await fs.readFile(defaultIniPath, 'utf8');
      } catch {}

      await fs.writeFile(iniPath, content, 'utf8');
      await commands.executeCommand('vscode.open', iniPath);
      window.showInformationMessage(`Created and opened new INI file: ${iniPath}`);

      let project = item.project ? item.project : item;
      if (!(project instanceof ProjectItem)) return;

      await project.setIni(iniPath);
    }

    private static async configureOrCreateIni(item: BaseFileItem): Promise<void> {
      if (!item.projectExe) {
        window.showWarningMessage(`No executable for: ${item.label} - cannot create INI file.`);
        return;
      }
      if (item.projectIni)
        try {
          await fs.access(item.projectIni.fsPath);
          // File exists, open it for editing
          await commands.executeCommand('vscode.open', item.projectIni);
          window.showInformationMessage(`Opened existing INI file: ${item.projectIni.fsPath}`);
          return;
        } catch {
          // File doesn't exist, fall through to create it
        }

      await this.createIniFile(item);
    }

    private static async selectProject(item: BaseFileItem): Promise<void> {
      const config = Runtime.configEntity;
      config.selectedProject = item.project.entity;
      await Runtime.db.save(config);
      await Runtime.projects.workspacesTreeView.refresh();
      await Runtime.projects.groupProjectTreeView.refresh();
      window.showInformationMessage(`Selected project: ${item.label}`);
    }
  }

  export class Compiler {
    public static get registers(): Disposable[] {
      return [commands.registerCommand(PROJECTS.COMMAND.SELECT_COMPILER, this.selectCompilerConfiguration.bind(this))];
    }

    public static async selectCompilerConfiguration(): Promise<void> {
      const configurations = Runtime.compilerConfigurations;

      if (!configurations.length) {
        window.showErrorMessage('No compiler configurations found. Please configure Delphi compiler settings.');
        return;
      }

      const items = configurations.map((config) => ({
        label: config.name,
        description: config.rsVarsPath,
        detail: `MSBuild: ${config.msBuildPath}`
      }));

      const selected = await window.showQuickPick(items, {
        placeHolder: 'Select Delphi Compiler Configuration',
        matchOnDescription: true,
        matchOnDetail: true
      });

      if (!selected) return;

      const config = Runtime.configEntity;
      config.groupProjectsCompiler = selected.label;
      await Runtime.db.save(config);
      await Runtime.projects.compilerStatusBarItem.updateDisplay();
      window.showInformationMessage(`Compiler configuration set to: ${selected?.label}`);
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
      Runtime.projects.workspacesTreeView.refresh();
      Runtime.projects.groupProjectTreeView.refresh();
    }

    private static async pickGroupProject(): Promise<void> {
      const uri = await Runtime.projects.groupProjectPicker.pickGroupProject();
      if (!uri) return;
      let groupProject = await Runtime.db.getGroupProject(uri.fsPath);
      if (!groupProject) {
        const projects = await new ProjectFileDiscovery().findFilesFromGroupProj(uri);
        groupProject = await Runtime.db.addGroupProject(basenameNoExt(uri), uri.fsPath, projects);
      }
      const config = Runtime.configEntity;
      config.selectedGroupProject = groupProject;
      if (config.selectedProject && !groupProject.projects.some((link) => link.project.id === config.selectedProject?.id))
        config.selectedProject = null;

      await Runtime.db.save(config);
      await Runtime.projects.groupProjectTreeView.refresh();
      window.showInformationMessage(`Loaded group project: ${groupProject?.name || uri.fsPath}`);
    }

    private static async unloadGroupProject(): Promise<void> {
      const config = Runtime.configEntity;
      if (config.selectedProject && config.selectedGroupProject?.projects.some((link) => link.project.id === config.selectedProject?.id))
        config.selectedProject = null;

      config.selectedGroupProject = null;
      await Runtime.db.save(config);
      await Runtime.projects.groupProjectTreeView.refresh();
      window.showInformationMessage('Unloaded group project. Showing default projects (if discovery is enabled).');
    }

    private static async editDefaultIni(): Promise<void> {
      const defaultIniPath = Runtime.extension.asAbsolutePath('dist/default.ini');
      try {
        await commands.executeCommand('vscode.open', Uri.file(defaultIniPath));
      } catch (error) {
        window.showErrorMessage(`Failed to open default.ini: ${error}`);
      }
    }
  }

  export class Configuration {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.ADD_PROJECT, this.addProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.REMOVE_PROJECT, this.removeProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.ADD_WORKSPACE, this.addWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RENAME_WORKSPACE, this.renameWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.REMOVE_WORKSPACE, this.removeWorkspace.bind(this))
      ];
    }

    private static async addProject(item: TreeItem): Promise<void> {
      const itemInWorkspaceContext = (i: TreeItem) => {
        return i instanceof WorkspaceItem || (i instanceof BaseFileItem && i.project.link.linkType === ProjectLinkType.Workspace);
      };
      if (!assertError(itemInWorkspaceContext(item), 'This command only works when invoked inside the context of a workspace tree item.')) return;

      const ws = item instanceof WorkspaceItem ? item.workspace : (item as BaseFileItem).project.link.workspaceSafe;

      if (!assertError(ws, 'Selected workspace not found.')) return;

      const uris = await window.showOpenDialog({
        canSelectMany: true,
        title: 'Add Delphi Projects',
        canSelectFolders: false,
        canSelectFiles: true,
        openLabel: 'Add',
        filters: {
          'Delphi Project Files': ['dproj', 'dpr', 'dpk']
        }
      });
      if (!assertError(uris, 'No project files selected')) return;
      await Promise.all(
        uris!.map(async (uri) => {
          if (!uri) return;
          const projectName = basenameNoExt(uri);
          let files = await new ProjectFileDiscovery().findFiles(Uri.file(dirname(uri.fsPath)), basenameNoExt(uri));
          if (files.isEmpty) return;
          const project = new Entities.Project();
          project.name = projectName;
          project.path = join(dirname(uri.fsPath), projectName);
          project.dproj = files.dproj?.fsPath || null;
          project.dpr = files.dpr?.fsPath || null;
          project.dpk = files.dpk?.fsPath || null;
          project.exe = files.exe?.fsPath || null;
          project.ini = files.ini?.fsPath || null;
          const link = new Entities.WorkspaceLink();
          link.project = project;
          link.workspace = ws!;
          ws!.projects.push(link);
        })
      );
      ws!.projects = new LexoSorter(ws!.projects).items;
      await Runtime.db.save(ws!);
      Runtime.projects.workspacesTreeView.refresh();
    }

    private static async removeProject(item: BaseFileItem): Promise<void> {
      const project = item.project.entity;
      const link = item.project.link;
      if (!assertError(link.linkType === ProjectLinkType.Workspace, 'Only workspace projects can be removed.')) return;

      const confirm = await window.showWarningMessage(
        `Are you sure you want to remove project ${project.name}? This will not delete any files.`,
        { modal: true },
        'Remove'
      );
      if (confirm !== 'Remove') return;
      await Runtime.db.removeProjectLink(link);
      await Runtime.projects.workspacesTreeView.refresh();
      window.showInformationMessage(`Removed project: ${item.label}`);
    }

    private static checkWorkspaceName(name: string, config: Entities.Configuration): string | undefined {
      if (!name || !name.trim()) return 'Workspace name cannot be empty';
      else if (config.workspaces.some((ws) => ws.name.toLowerCase() === name.trim().toLowerCase()))
        return 'A workspace with this name already exists';
    }

    private static async addWorkspace(): Promise<void> {
      const config = Runtime.configEntity;
      const name = await window.showInputBox({
        prompt: 'Enter a name for the new workspace',
        placeHolder: 'Workspace Name',
        validateInput: (value) => this.checkWorkspaceName(value, config)
      });
      if (!assertError(name, 'Cannot create Workspace without name.')) return;
      const compilerName = await window.showQuickPick(
        Runtime.compilerConfigurations.map((cfg) => ({
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
      if (!assertError(compilerName, 'Cannot create Workspace without compiler configuration.')) return;
      const workspace = new Entities.Workspace();
      workspace.name = name!.trim();
      workspace.compiler = compilerName!.label;
      config.workspaces.push(workspace);
      config.workspaces = new LexoSorter(config.workspaces).items;
      await Runtime.db.save(config);
      await Runtime.projects.workspacesTreeView.refresh();
      window.showInformationMessage(`Created new workspace: ${workspace.name}`);
    }

    private static async removeWorkspace(item: WorkspaceItem): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) return;

      const ws = item.workspace;
      await Runtime.db.removeWorkspace(ws);
      await Runtime.projects.workspacesTreeView.refresh();
      window.showInformationMessage(`Removed workspace: ${ws.name}`);
    }

    private static async renameWorkspace(item: WorkspaceItem): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) return;

      const ws = item.workspace;
      const config = Runtime.configEntity;
      const newName = await window.showInputBox({
        prompt: 'Enter a new name for the workspace',
        placeHolder: 'Workspace Name',
        value: ws.name,
        validateInput: (value) => this.checkWorkspaceName(value, config)
      });
      if (!assertError(newName && newName.trim() && newName.trim() !== ws.name, 'Workspace name not changed.')) return;
      ws.name = newName!.trim();
      await Runtime.db.save(ws);
      await Runtime.projects.workspacesTreeView.refresh();
      window.showInformationMessage(`Renamed workspace to: ${ws.name}`);
    }
  }
}
