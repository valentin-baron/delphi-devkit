import { TreeDragAndDropController, DataTransfer, DataTransferItem, TreeItem, window } from 'vscode';
import { Runtime } from '../../runtime';
import { ProjectItem } from './items/project';
import { Entities } from '../entities';
import { assertError } from '../../utils';
import { WorkspaceItem } from './items/workspaceItem';
import { PROJECTS } from '../../constants';
import { BaseFileItem, MainProjectItem } from './items/baseFile';
import { Option } from '../../types';

interface Target {
  isEmpty: boolean;
  isProject: boolean;
  isWorkspace: boolean;
  entity: {
    project?: Option<Entities.Project>;
    workspace?: Option<Entities.Workspace>;
    workspaceLink?: Option<Entities.ProjectLink>;
  };
  item: {
    project?: Option<MainProjectItem>;
    workspace?: Option<WorkspaceItem>;
  };
}

interface Source extends Target {
  isDraggedFromGroupProject: boolean;
  isFSFileList?: boolean;
  fileList?: string[];
}

class ExtendedTransferInfo {
  public readonly source: Source;
  public readonly target: Target;
  constructor(source: TreeItem, target: TreeItem | undefined) {
    const evaluatedSource = this.evaluate(source);
    this.source = {
      isDraggedFromGroupProject: evaluatedSource.isProject && !evaluatedSource.entity.workspace,
      ...this.evaluate(source)
    };
    this.target = this.evaluate(target);
  }

  public validate(): boolean {
    if (!this.target.entity.workspace || !this.target.item.workspace) return false;
    if (this.source.isProject) {
      if (this.target.isEmpty) return false;
      if (!this.source.entity.project || !this.source.item.project) return false;
      if (!this.source.isDraggedFromGroupProject)
        // we are dragging from a workspace - all workspace info must exist
        if (!this.source.entity.workspaceLink || !this.source.entity.workspace || !this.source.item.workspace)
          return false;
    }
    if (this.target.isProject && (!this.target.entity.project || !this.target.item.project || !this.target.entity.workspaceLink)) return false;

    if (this.source.isWorkspace) {
      if (!this.source.entity.workspace || !this.source.item.workspace) return false;
      if (!this.target.entity.workspace || !this.target.item.workspace) return false;
    }
    if (this.target.isWorkspace) {
      if (!this.target.entity.workspace || !this.target.item.workspace) return false;
      if (!this.source.isDraggedFromGroupProject && (this.source.entity.workspace!.id === this.target.entity.workspace!.id)) return false;
    }
    return true;
  }

  private evaluate(target: TreeItem | undefined): Target {
    if (!target)
      return {
        isEmpty: true,
        isProject: false,
        isWorkspace: false,
        entity: {},
        item: {}
      };

    const isProject = target instanceof BaseFileItem;
    const isWorkspace = target instanceof WorkspaceItem;
    let projectItem: Option<MainProjectItem>;
    let workspaceItem: Option<WorkspaceItem>;
    let projectEntity: Option<Entities.Project>;
    let workspaceEntity: Option<Entities.Workspace> = undefined;
    let linkEntity: Option<Entities.ProjectLink>;
    if (isProject) {
      projectItem = target.project;
      projectEntity = target.project.entity;
      if (Runtime.projects.workspacesTreeView.projects.find((item) => item.link.id === target.project.link.id)) {
        workspaceItem = Runtime.projects.workspacesTreeView.getWorkspaceItemByTreeItem(target);
        workspaceEntity = target.project.link.workspace;
        linkEntity = target.project.link as Entities.ProjectLink;
      }
    }
    if (isWorkspace) {
      workspaceItem = target;
      workspaceEntity = target.workspace;
    }
    return {
      isEmpty: false,
      isProject: isProject,
      isWorkspace: isWorkspace,
      entity: {
        project: projectEntity,
        workspace: workspaceEntity,
        workspaceLink: linkEntity
      },
      item: {
        project: projectItem,
        workspace: workspaceItem
      }
    };
  }
}

export class WorkspaceTreeDragDropController implements TreeDragAndDropController<TreeItem> {
  public readonly dragMimeTypes = [PROJECTS.MIME_TYPES.WORKSPACE, PROJECTS.MIME_TYPES.WORKSPACE_PROJECT];
  public readonly dropMimeTypes = [
    PROJECTS.MIME_TYPES.WORKSPACE,
    PROJECTS.MIME_TYPES.WORKSPACE_PROJECT,
    PROJECTS.MIME_TYPES.GROUP_PROJECT_CHILD,
    PROJECTS.MIME_TYPES.FS_FILES
  ];

  public async handleDrag(source: TreeItem[], dataTransfer: DataTransfer): Promise<void> {
    if (!source || source.length !== 1) return;
    const item = source[0];
    if (
      !assertError(
        item instanceof ProjectItem || item instanceof WorkspaceItem,
        'Invalid item type. Drag and drop is only supported for Delphi projects and workspace items.'
      )
    )
      return;

    if (item instanceof ProjectItem) dataTransfer.set(PROJECTS.MIME_TYPES.WORKSPACE_PROJECT, new DataTransferItem(item.link.id));
    else if (item instanceof WorkspaceItem) dataTransfer.set(PROJECTS.MIME_TYPES.WORKSPACE, new DataTransferItem(item.workspace.id));
  }

  public async handleDrop(target: TreeItem | undefined, dataTransfer: DataTransfer): Promise<void> {
    let hasFiles = false;
    for (const [mime, item] of dataTransfer) {
      if (!item?.value) continue;
      switch(mime) {
        case PROJECTS.MIME_TYPES.WORKSPACE_PROJECT:
          return await this.handleDropProject(target, item);
        case PROJECTS.MIME_TYPES.WORKSPACE:
          return await this.handleDropWorkspace(target, item);
        case PROJECTS.MIME_TYPES.GROUP_PROJECT_CHILD:
          return await this.handleDropProject(target, item, {
            sourceIsGroupProject: true
          });
        case PROJECTS.MIME_TYPES.FS_FILES:
          hasFiles = true;
      }
    }
    // if we reach this, it means we haven't handled any other type
    if (hasFiles) await window.showInformationMessage('Drag-Drop of files from file system is coming soon.');
  }

  private async handleDropProject(
    target: TreeItem | undefined,
    transferItem: DataTransferItem,
    options?: { sourceIsGroupProject: boolean }
  ): Promise<void> {
    const id =
      typeof transferItem.value === 'number' ? transferItem.value : typeof transferItem.value === 'string' ? parseInt(transferItem.value) : NaN;
    if (isNaN(id)) return;
    let source: ProjectItem | undefined;
    if (options?.sourceIsGroupProject) source = Runtime.projects.groupProjectTreeView.projects.find((proj) => proj.link.id === id);
    else source = Runtime.projects.workspacesTreeView.projects.find((proj) => proj.link.id === id);
    if (!source) return;
    const transfer = new ExtendedTransferInfo(source, target);
    if (transfer.source.isProject) await this.dropProject(transfer);
    await Runtime.projects.workspacesTreeView.refresh();
  }

  private async handleDropWorkspace(target: TreeItem | undefined, transferItem: DataTransferItem): Promise<void> {
    const id =
      typeof transferItem.value === 'number' ? transferItem.value : typeof transferItem.value === 'string' ? parseInt(transferItem.value) : NaN;
    if (isNaN(id)) return;
    const source = Runtime.projects.workspacesTreeView.workspaceItems.find((ws) => ws.workspace.id === id);
    if (!source) return;
    const transfer = new ExtendedTransferInfo(source, target);
    if (transfer.source.isWorkspace) await this.dropWorkspace(transfer);
    await Runtime.projects.workspacesTreeView.refresh();
  }

  private async dropProject(transfer: ExtendedTransferInfo): Promise<void> {
    // validate all required combinations
    if (!transfer.validate()) return;
    const source = transfer.source;
    const target = transfer.target;
    if (source.isDraggedFromGroupProject)
      return await Runtime.client.applyChanges([
        {
          type: 'AddProject',
          project_id: source.entity.project!.id,
          workspace_id: target.entity.workspace!.id
        }
      ]);

    await Runtime.client.applyChanges([
      {
        type: 'MoveProject',
        project_link_id: source.entity.workspaceLink!.id,
        drop_target: target.isProject ? target.entity.workspaceLink!.id : target.entity.workspace!.id
      }
    ]);
  }

  private async dropWorkspace(transfer: ExtendedTransferInfo): Promise<void> {
    const source = transfer.source;
    const target = transfer.target;
    await Runtime.client.applyChanges([
      {
        type: 'MoveWorkspace',
        workspace_id: source.entity.workspace!.id,
        drop_target: target.entity.workspace!.id
      }
    ]);
  }
}

export class GroupProjectTreeDragDropController implements TreeDragAndDropController<TreeItem> {
  public readonly dragMimeTypes = [PROJECTS.MIME_TYPES.GROUP_PROJECT_CHILD];
  public readonly dropMimeTypes = [];

  public async handleDrag(source: BaseFileItem[], dataTransfer: DataTransfer): Promise<void> {
    if (!source || source.length !== 1) return;
    const item = source[0];
    if (!item.isProjectItem) return;
    dataTransfer.set(PROJECTS.MIME_TYPES.GROUP_PROJECT_CHILD, new DataTransferItem(item.project.link.id));
  }

  public async handleDrop(): Promise<void> {
    window.showInformationMessage('Group project items cannot be adjusted.');
  }
}
