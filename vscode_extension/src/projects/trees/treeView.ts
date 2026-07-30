import { TreeItem, EventEmitter, Event, workspace, ConfigurationChangeEvent, TreeDataProvider, window, Uri } from 'vscode';
import { BaseFileItem } from './items/baseFile';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectItem } from './items/project';
// import { DelphiProjectsDragAndDropController } from "./DragAndDropController";
import { Runtime } from '../../runtime';
import { PROJECTS } from '../../constants';
import { TreeItemDecorator } from './treeItemDecorator';
import { WorkspaceItem } from './items/workspaceItem';
import { GroupProjectTreeDragDropController, WorkspaceTreeDragDropController } from './dragAndDrop';
import { ConfigurationGroupItem, PlatformGroupItem } from './items/configurationItem';

type NullableTreeItem = BaseFileItem | undefined | null | void;

export abstract class DelphiProjectsTreeView implements TreeDataProvider<TreeItem> {
  private changeEventEmitter: EventEmitter<NullableTreeItem> = new EventEmitter<NullableTreeItem>();
  public readonly onDidChangeTreeData: Event<NullableTreeItem> = this.changeEventEmitter.event;
  public projects: ProjectItem[] = [];

  private createWatchers(): void {
    const dprojWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Rr][Oo][Jj]', false, true);
    const dprWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Rr]', false, true);
    const dpkWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Kk]', false, true);
    const exeWatcher = workspace.createFileSystemWatcher('**/*.[Ee][Xx][Ee]', false, true);
    const iniWatcher = workspace.createFileSystemWatcher('**/*.[Ii][Nn][Ii]', false, true);
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
    for (const item of this.projects)
      if (
        item.entity.directory === file.fsPath ||
        item.projectDproj?.fsPath === file.fsPath ||
        item.projectDpr?.fsPath === file.fsPath ||
        item.projectDpk?.fsPath === file.fsPath ||
        item.projectExe?.fsPath === file.fsPath ||
        item.projectIni?.fsPath === file.fsPath
      )
        return true;

    return false;
  }

  private onWatcherEvent(file: Uri): void {
    if (this.isRelevantFile(file)) this.refresh();
  }

  private createConfigurationWatcher() {
    workspace.onDidChangeConfiguration((event: ConfigurationChangeEvent) => {
      if (
        event.affectsConfiguration(PROJECTS.CONFIG.full(PROJECTS.CONFIG.DISCOVERY.PROJECT_PATHS)) ||
        event.affectsConfiguration(PROJECTS.CONFIG.full(PROJECTS.CONFIG.DISCOVERY.EXCLUDE_PATTERNS)) ||
        event.affectsConfiguration(PROJECTS.CONFIG.full(PROJECTS.CONFIG.CONFIG_PLATFORM_DISPLAY))
      )
        this.refresh();
    });
  }

  constructor() {
    this.createWatchers();
    this.createConfigurationWatcher();
  }

  getTreeItem(element: TreeItem): TreeItem {
    return element;
  }

  private async createChildrenForProject(project: ProjectItem): Promise<TreeItem[]> {
    const fileChildren: BaseFileItem[] = [];
    await project.createChild(DelphiProjectTreeItemType.DprojFile, fileChildren);
    await project.createChild(DelphiProjectTreeItemType.DprFile, fileChildren);
    await project.createChild(DelphiProjectTreeItemType.DpkFile, fileChildren);
    await project.createChild(DelphiProjectTreeItemType.ExecutableFile, fileChildren);
    await project.createChild(DelphiProjectTreeItemType.IniFile, fileChildren);

    const displayMode = workspace.getConfiguration(PROJECTS.CONFIG.KEY)
      .get<string>(PROJECTS.CONFIG.CONFIG_PLATFORM_DISPLAY, 'aboveFiles');

    if (displayMode === 'off') return fileChildren;

    // Fetch dproj metadata and add config/platform groups if there are options
    const configItems: TreeItem[] = [];
    try {
      const metadata = await Runtime.client.dprojMetadata(project.entity.id);
      project.dprojMetadata = metadata;
      if (metadata.configurations.length > 1) {
        configItems.push(new ConfigurationGroupItem(project.entity.id, project.link.id, metadata));
      }
      if (metadata.platforms.length > 1) {
        configItems.push(new PlatformGroupItem(project.entity.id, project.link.id, metadata));
      }
    } catch {
      // If metadata fetch fails, just show the file children
    }

    return displayMode === 'belowFiles'
      ? [...fileChildren, ...configItems]
      : [...configItems, ...fileChildren];
  }

  protected abstract loadTreeItemsFromDatabase(): Promise<TreeItem[]>;

  protected get itemsLoaded(): boolean {
    return !!this.projects && this.projects.length > 0;
  }

  protected get loadedItems(): TreeItem[] {
    return this.projects;
  }

  async getChildren(element?: TreeItem): Promise<TreeItem[]> {
    if (!element)
      if (this.itemsLoaded) return this.loadedItems;
      else return await this.loadTreeItemsFromDatabase();
    else if (element instanceof ProjectItem) return await this.createChildrenForProject(element);
    else if (element instanceof WorkspaceItem) return element.projects;
    else if (element instanceof ConfigurationGroupItem) return element.getChildren();
    else if (element instanceof PlatformGroupItem) return element.getChildren();

    return [];
  }

  public async refresh(): Promise<void> {
    this.projects = [];
    this.changeEventEmitter.fire(undefined);
  }
}

export class WorkspacesTreeView extends DelphiProjectsTreeView {
  public workspaceItems: WorkspaceItem[] = [];

  constructor(
    public readonly dragAndDropController = new WorkspaceTreeDragDropController(),
    public readonly treeItemDecorator = new TreeItemDecorator()
  ) {
    super();
    Runtime.extension.subscriptions.push(
      window.createTreeView(PROJECTS.VIEW.WORKSPACES, {
        treeDataProvider: this,
        dragAndDropController: this.dragAndDropController,
        showCollapseAll: true
      }),
      window.registerFileDecorationProvider(this.treeItemDecorator)
    );
  }

  protected get itemsLoaded(): boolean {
    return !!this.workspaceItems && this.workspaceItems.length > 0;
  }

  protected get loadedItems(): TreeItem[] {
    return this.workspaceItems;
  }

  protected async loadTreeItemsFromDatabase(): Promise<TreeItem[]> {
    let data = Runtime.projectsData;
    Runtime.updateProjectContexts();
    this.workspaceItems = data?.workspaces.map((ws) => new WorkspaceItem(ws)) || [];
    this.projects = this.workspaceItems.flatMap((ws) => ws.projects);
    return this.workspaceItems;
  }

  public async refresh(): Promise<void> {
    this.workspaceItems = [];
    await super.refresh();
  }

  public getWorkspaceItemByTreeItem(item: TreeItem): WorkspaceItem | undefined {
    if (item instanceof WorkspaceItem) return item;
    else if (item instanceof BaseFileItem)
      return this.workspaceItems.find((wsItem) => wsItem.projects.some((projItem) => projItem.link.id === item.project.link.id));
  }
}

export class GroupProjectTreeView extends DelphiProjectsTreeView {
  constructor(public readonly dragAndDropController = new GroupProjectTreeDragDropController()) {
    super();
    Runtime.extension.subscriptions.push(
      window.createTreeView(PROJECTS.VIEW.GROUP_PROJECT, {
        treeDataProvider: this,
        dragAndDropController: this.dragAndDropController,
        showCollapseAll: true
      })
    );
  }

  protected async loadTreeItemsFromDatabase(): Promise<TreeItem[]> {
    const groupProject = Runtime.projectsData?.group_project;
    Runtime.setContext(PROJECTS.CONTEXT.IS_GROUP_PROJECT_OPENED, !!groupProject);
    if (groupProject)
      for (const link of groupProject.project_links)
        if (Runtime.getProjectOfLink(link)) {
          const item = ProjectItem.fromData(link);
          this.projects.push(item);
        }

    return this.projects;
  }
}
