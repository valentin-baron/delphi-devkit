import { commands, env, Uri, window, Disposable, TreeItem } from 'vscode';
import { dirname, join } from 'path';
import { promises as fs } from 'fs';
import { Runtime } from '../runtime';
import { PROJECTS } from '../constants';
import { Coroutine, DelphiProjectTreeItemType } from '../types';
import { Entities } from './entities';
import { BaseFileItem } from './trees/items/baseFile';
import { ProjectItem } from './trees/items/project';
import { assertError, basenameNoExt } from '../utils';
import { WorkspaceItem } from './trees/items/workspaceItem';
import { Change } from '../client';
import { Option } from '../types';

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

    private static async selectedProjectAction(callback: Coroutine<void, [Entities.ProjectLink]>): Promise<void> {
      const project = Runtime.activeProject;
      if (!assertError(project, 'Could not evaluate selected project.')) return;
      const links = project!.links;
      if (!assertError(links.length > 0, 'Selected project has no associated project links.')) return;
      const link = project!.links[0] || null;
      if (!assertError(link, 'Could not find valid project link for the selected project.')) return;
      await callback(link!);
    }

    private static async compileSelectedProject() {
      await this.selectedProjectAction(async (link) => {
        await Runtime.compileProjectLink(link, false);
      });
    }

    private static async recreateSelectedProject() {
      await this.selectedProjectAction(async (link) => {
        await Runtime.compileProjectLink(link, true);
      });
    }

    private static async runSelectedProject() {
      await this.selectedProjectAction(async (link) => {
        const project = Runtime.getProjectOfLink(link);
        if (!project?.exe) {
          window.showWarningMessage('Selected project has no associated executable to run.');
          return;
        }
        try {
          // Use the system's default application handler to launch the executable
          await env.openExternal(Uri.file(project.exe));
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
        commands.registerCommand(PROJECTS.COMMAND.SELECT_PROJECT, this.selectProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.COMPILE_ALL_IN_WORKSPACE, this.compileAllInWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE_ALL_IN_WORKSPACE, this.recreateAllInWorkspace.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.COMPILE_ALL_FROM_HERE, this.compileAllFromHere.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE_ALL_FROM_HERE, this.recreateAllFromHere.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.SET_MANUAL_PATH, this.setManualPath.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.DISCOVER_PROJECT_PATHS, this.discoverProjectPaths.bind(this)),
      ];
    }

    private static async compile(item: BaseFileItem): Promise<void> {
      await Runtime.compileProjectLink(item.project.link, false);
    }

    private static async recreate(item: BaseFileItem): Promise<void> {
      await Runtime.compileProjectLink(item.project.link, true);
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
      if (!assertError(item.projectExe, `No executable for: ${item.label} - cannot create INI file.`)) return;
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

      await Runtime.client.applyChanges([{
        type: 'UpdateProject',
        project_id: project.entity.id,
        data: {
          ini: iniPath
        }
      }]);
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
      await Runtime.client.applyChanges([
        {
          type: 'SelectProject',
          project_id: item.project.entity.id
        }
      ]);
      window.showInformationMessage(`Selected project: ${item.label}`);
    }

    private static async compileAllInWorkspace(item: TreeItem): Promise<void> {
      const wsItem = Runtime.projects.workspacesTreeView.getWorkspaceItemByTreeItem(item);
      if (!assertError(wsItem?.workspace, 'Could not determine workspace from the selected item.')) return;
      await Runtime.client.compileAllInWorkspace(false, wsItem!.workspace.id);
    }

    private static async recreateAllInWorkspace(item: TreeItem): Promise<void> {
      const wsItem = Runtime.projects.workspacesTreeView.getWorkspaceItemByTreeItem(item);
      if (!assertError(wsItem?.workspace, 'Could not determine workspace from the selected item.')) return;
      await Runtime.client.compileAllInWorkspace(true, wsItem!.workspace.id);
    }

    private static async compileAllFromHere(item: BaseFileItem): Promise<void> {
      await Runtime.client.compileFromLink(false, item.project.link.id);
    }

    private static async recreateAllFromHere(item: BaseFileItem): Promise<void> {
      await Runtime.client.compileFromLink(true, item.project.link.id);
    }

    private static async setManualPath(item: BaseFileItem): Promise<void> {
      let fileType = '';
      switch (item.itemType) {
        case DelphiProjectTreeItemType.DprojFile:
          fileType = 'dproj';
          break;
        case DelphiProjectTreeItemType.DprFile:
          fileType = 'dpr';
          break;
        case DelphiProjectTreeItemType.DpkFile:
          fileType = 'dpk';
          break;
        case DelphiProjectTreeItemType.ExecutableFile:
          fileType = 'exe';
          break;
        case DelphiProjectTreeItemType.IniFile:
          fileType = 'ini';
          break;
        default:
          window.showErrorMessage('This command can only be used on project file items.');
          return;
      }
      const uri = await window.showOpenDialog({
        canSelectMany: false,
        title: `Select new path for ${item.project.entity.name}.${fileType} file`,
        filters: {
          [`${fileType.toUpperCase()} files`]: [fileType],
        }
      });
      if (!uri) return;
      await Runtime.client.applyChanges([
        {
          type: 'UpdateProject',
          project_id: item.project.entity.id,
          data: {
            [fileType]: uri[0].fsPath
          }
        }
      ]);
    }

    private static async discoverProjectPaths(item: BaseFileItem): Promise<void> {
      const project = item.project;
      if (!assertError(project, 'Could not determine project for the selected item.')) return;
      await Runtime.client.applyChanges([
        {
          type: 'RefreshProject',
          project_id: project.entity.id
        }
      ]);
    }
  }

  export class Compiler {
    public static get registers(): Disposable[] {
      return [commands.registerCommand(PROJECTS.COMMAND.SELECT_COMPILER, this.selectCompilerConfiguration.bind(this))];
    }

    public static async selectCompilerConfiguration(): Promise<void> {
      const configurations = Runtime.compilerConfigurations;

      if (Object.keys(configurations).length <= 0) {
        window.showErrorMessage('No compiler configurations found.');
        return;
      }

      // we need to use both keys and values, so we map them to an array of objects
      const items = Object.entries(configurations).sort(([keyA, configA], [keyB, configB]) =>
        configB.compiler_version - configA.compiler_version
      ).map(([key, config]) => ({
        label: config.product_name,
        description: config.installation_path,
        detail: key
      }));

      const selected = await window.showQuickPick(items, {
        placeHolder: 'Select Delphi Compiler Configuration',
        matchOnDescription: true,
        matchOnDetail: true
      });

      if (!selected) return;

      const success = await Runtime.client.applyChanges([
        {
          type: 'SetGroupProjectCompiler',
          compiler: selected.detail
        }
      ]);
      if (success) window.showInformationMessage(`Compiler configuration set to: ${selected?.label}`);
    }
  }

  export class ProjectsTreeView {
    public static get registers(): Disposable[] {
      return [
        commands.registerCommand(PROJECTS.COMMAND.SELECT_GROUP_PROJECT, this.pickGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.UNLOAD_GROUP_PROJECT, this.unloadGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.EDIT_DEFAULT_INI, this.editDefaultIni.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.COMPILE_ALL_IN_GROUP_PROJECT, this.compileAllInGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.RECREATE_ALL_IN_GROUP_PROJECT, this.recreateAllInGroupProject.bind(this)),
        commands.registerCommand(PROJECTS.COMMAND.REFRESH, this.refresh.bind(this)),
      ];
    }

    private static async pickGroupProject(): Promise<void> {
      const uri = await Runtime.projects.groupProjectPicker.pickGroupProject();
      if (!uri) return;
      await Runtime.client.applyChanges([
        {
          type: 'SetGroupProject',
          groupproj_path: uri.fsPath,
        }
      ]);
      window.showInformationMessage(`Loaded group project: ${uri.fsPath}`);
    }

    private static async unloadGroupProject(): Promise<void> {
      await Runtime.client.applyChanges([
        {
          type: 'RemoveGroupProject',
        }
      ]);
      window.showInformationMessage('Unloaded group project.');
    }

    private static async editDefaultIni(): Promise<void> {
      const defaultIniPath = Runtime.extension.asAbsolutePath('dist/default.ini');
      try {
        await commands.executeCommand('vscode.open', Uri.file(defaultIniPath));
      } catch (error) {
        window.showErrorMessage(`Failed to open default.ini: ${error}`);
      }
    }

    private static async compileAllInGroupProject(item: TreeItem): Promise<void> {
      await Runtime.client.compileAllInGroupProject(false);
    }

    private static async recreateAllInGroupProject(item: TreeItem): Promise<void> {
      await Runtime.client.compileAllInGroupProject(true);
    }

    private static async refresh(): Promise<void> {
      await Runtime.client.refresh();
      await Runtime.projects.workspacesTreeView.refresh();
      await Runtime.projects.groupProjectTreeView.refresh();
      await Runtime.projects.compilerStatusBarItem.updateDisplay();
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
        return i instanceof WorkspaceItem || (i instanceof BaseFileItem && !!Runtime.getWorkspaceOfLink(i.project.link));
      };
      if (!assertError(itemInWorkspaceContext(item), 'This command only works when invoked inside the context of a workspace tree item.')) return;

      const ws = item instanceof WorkspaceItem ? item.workspace : Runtime.getWorkspaceOfLink((item as BaseFileItem).project.link);

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
      const changes: Change[] = [];
      for (const uri of uris!)
        changes.push({
          type: 'NewProject',
          file_path: uri.fsPath,
          workspace_id: ws!.id
        });
      await Runtime.client.applyChanges(changes);
    }

    private static async removeProject(item: BaseFileItem): Promise<void> {
      const project = item.project.entity;
      const link = item.project.link;
      if (!assertError(!!Runtime.getWorkspaceOfLink(link), 'Only workspace projects can be removed.')) return;

      const confirm = await window.showWarningMessage(
        `Are you sure you want to remove project ${project.name}? This will not delete any files.`,
        { modal: true },
        'Remove'
      );
      if (confirm !== 'Remove') return;
      await Runtime.client.applyChanges([
        {
          type: 'RemoveProject',
          project_link_id: link.id
        }
      ]);
      window.showInformationMessage(`Removed project: ${item.label}`);
    }

    private static checkWorkspaceName(name: string, data?: Option<Entities.ProjectsData>): string | undefined {
      if (!name || !name.trim()) return 'Workspace name cannot be empty';
      else if (data && data.workspaces.some((ws) => ws.name.toLowerCase() === name.trim().toLowerCase()))
        return 'A workspace with this name already exists';
    }

    private static async addWorkspace(): Promise<void> {
      const data = Runtime.projectsData;
      const name = await window.showInputBox({
        prompt: 'Enter a name for the new workspace',
        placeHolder: 'Workspace Name',
        validateInput: (value) => this.checkWorkspaceName(value, data)
      });
      if (!assertError(name, 'Cannot create Workspace without name.')) return;
      const items = Object.entries(Runtime.compilerConfigurations).sort(([keyA, configA], [keyB, configB]) =>
        configB.compiler_version - configA.compiler_version
      ).map(([key, cfg]) => ({
        label: cfg.product_name,
        description: cfg.installation_path,
        detail: key
      }));
      const compilerName = await window.showQuickPick(
        items,
        {
          placeHolder: 'Select Delphi Compiler Configuration for this workspace',
          matchOnDescription: false,
          matchOnDetail: false,
          canPickMany: false
        }
      );
      if (!assertError(compilerName, 'Cannot create Workspace without compiler configuration.')) return;
      await Runtime.client.applyChanges([
        {
          type: 'AddWorkspace',
          name: name!.trim(),
          compiler: compilerName!.detail
        }
      ]);
      window.showInformationMessage(`Created new workspace: ${name!.trim()}`);
    }

    private static async removeWorkspace(item: WorkspaceItem): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) return;

      const ws = item.workspace;
      await Runtime.client.applyChanges([
        {
          type: 'RemoveWorkspace',
          workspace_id: ws.id
        }
      ]);
      window.showInformationMessage(`Removed workspace: ${ws.name}`);
    }

    private static async renameWorkspace(item: WorkspaceItem): Promise<void> {
      if (!assertError(item instanceof WorkspaceItem, 'This command can only be invoked on a workspace tree item.')) return;

      const ws = item.workspace;
      const data = Runtime.projectsData;
      const newName = await window.showInputBox({
        prompt: 'Enter a new name for the workspace',
        placeHolder: 'Workspace Name',
        value: ws.name,
        validateInput: (value) => this.checkWorkspaceName(value, data)
      });
      if (!assertError(newName && newName.trim() && newName.trim() !== ws.name, 'Workspace name not changed.')) return;
      await Runtime.client.applyChanges([
        {
          type: 'UpdateWorkspace',
          workspace_id: ws.id,
          data: {
            name: newName!.trim()
          }
        }
      ]);
      window.showInformationMessage(`Renamed workspace ${ws.name} to: ${newName!.trim()}`);
    }
  }
}
