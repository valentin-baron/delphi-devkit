import {
  TreeItem,
  EventEmitter,
  Event,
  workspace,
  ConfigurationChangeEvent,
  TreeDataProvider,
  window,
  Uri,
} from "vscode";
import { DelphiProjectTreeItem } from "./delphiProjectTreeItem";
import { DelphiProjectTreeItemType } from "../../types";
import { DelphiProject } from "./delphiProject";
// import { DelphiProjectsDragAndDropController } from "./DragAndDropController";
import { Runtime } from "../../runtime";
import { PROJECTS } from "../../constants";
import { TreeItemDecorator } from "./treeItemDecorator";
import { WorkspaceItem } from "./workspaceItem";

type NullableTreeItem = DelphiProjectTreeItem | undefined | null | void;

export abstract class DelphiProjectsTreeView
  implements TreeDataProvider<TreeItem>
{
  private changeEventEmitter: EventEmitter<NullableTreeItem> = new EventEmitter<NullableTreeItem>();
    public readonly onDidChangeTreeData: Event<NullableTreeItem> = this.changeEventEmitter.event;
  public items: DelphiProject[] = [];

  private createWatchers(): void {
    const dprojWatcher = workspace.createFileSystemWatcher("**/*.[Dd][Pp][Rr][Oo][Jj]", false, true);
    const dprWatcher = workspace.createFileSystemWatcher("**/*.[Dd][Pp][Rr]", false, true);
    const dpkWatcher = workspace.createFileSystemWatcher("**/*.[Dd][Pp][Kk]", false, true);
    const exeWatcher = workspace.createFileSystemWatcher("**/*.[Ee][Xx][Ee]", false, true);
    const iniWatcher = workspace.createFileSystemWatcher("**/*.[Ii][Nn][Ii]", false, true);
    const watchers = [dprojWatcher, dprWatcher, dpkWatcher, iniWatcher, exeWatcher];

    watchers.forEach((watcher) => {
      watcher.onDidCreate((file: Uri) => {
        this.onWatcherEvent(file);
      });
      watcher.onDidDelete((file: Uri) => {
        this.onWatcherEvent(file);
      });
    });
    Runtime.extension.subscriptions.push(...watchers);
  }

  private isRelevantFile(file: Uri): boolean {
    for (const item of this.items) {
      if (
        item.entity.path === file.fsPath ||
        item.projectDproj?.fsPath === file.fsPath ||
        item.projectDpr?.fsPath === file.fsPath ||
        item.projectDpk?.fsPath === file.fsPath ||
        item.projectExe?.fsPath === file.fsPath ||
        item.projectIni?.fsPath === file.fsPath
      ) {
        return true;
      }
    }
    return false;
  }

  private onWatcherEvent(file: Uri): void {
    if (this.isRelevantFile(file)) {
      this.refreshTreeView();
    }
  }

  private createConfigurationWatcher() {
    workspace.onDidChangeConfiguration((event: ConfigurationChangeEvent) => {
      if (
        event.affectsConfiguration(PROJECTS.CONFIG.full(PROJECTS.CONFIG.DISCOVERY.PROJECT_PATHS)) ||
        event.affectsConfiguration(PROJECTS.CONFIG.full(PROJECTS.CONFIG.DISCOVERY.EXCLUDE_PATTERNS))
      ) {
        this.refreshTreeView();
      }
    });
  }

  constructor() {
    this.createWatchers();
    this.createConfigurationWatcher();
  }

  getTreeItem(element: TreeItem): TreeItem {
    return element;
  }

  private createChildrenForProject(
    project: DelphiProject
  ): DelphiProjectTreeItem[] {
    const children: DelphiProjectTreeItem[] = [];
    project.createChild(DelphiProjectTreeItemType.DprojFile, children);
    project.createChild(DelphiProjectTreeItemType.DprFile, children);
    project.createChild(DelphiProjectTreeItemType.DpkFile, children);
    project.createChild(DelphiProjectTreeItemType.ExecutableFile, children);
    project.createChild(DelphiProjectTreeItemType.IniFile, children);
    return children;
  }

  protected abstract loadTreeItemsFromDatabase(): Promise<TreeItem[]>;

  async getChildren(
    element?: TreeItem
  ): Promise<TreeItem[]> {
    if (!element) {
      return this.loadTreeItemsFromDatabase();
    } else if (element instanceof DelphiProject) {
      return this.createChildrenForProject(element);
    } else if (element instanceof WorkspaceItem) {
      return element.projects;
    }
    return [];
  }

  public async refreshTreeView(): Promise<void> {
    this.changeEventEmitter.fire(undefined);
  }
}

export class WorkspacesTreeView extends DelphiProjectsTreeView {
  public workspaceItems: WorkspaceItem[] = [];

  constructor(
    // public readonly dragAndDropController = new DelphiProjectsDragAndDropController(),
    public readonly treeItemDecorator = new TreeItemDecorator()
  ) {
    super();
    Runtime.extension.subscriptions.push(
      window.registerFileDecorationProvider(this.treeItemDecorator)
    );
  }

  public async loadTreeItemsFromDatabase(): Promise<TreeItem[]>  {
    const config = await Runtime.db.getConfiguration();
    Runtime.setContext(PROJECTS.CONTEXT.IS_PROJECT_SELECTED, !!config.selectedProject);
    Runtime.setContext(PROJECTS.CONTEXT.DOES_SELECTED_PROJECT_HAVE_EXE, !!config.selectedProject?.exe);
    this.workspaceItems = config?.workspaces.map(ws => new WorkspaceItem(ws)) || [];
    return this.workspaceItems;
  }
}

export class GroupProjectTreeView extends DelphiProjectsTreeView {
  protected async loadTreeItemsFromDatabase(): Promise<TreeItem[]> {
    const groupProject = (await Runtime.db.getConfiguration()).selectedGroupProject;
    Runtime.setContext(PROJECTS.CONTEXT.IS_GROUP_PROJECT_OPENED, !!groupProject);
    const result: DelphiProject[] = [];
    if (groupProject) {
      for (const link of groupProject.projects) {
        if (link.project) {
          const item = DelphiProject.fromData(link);
          result.push(item);
        }
      }
    }
    return result;
  }
}