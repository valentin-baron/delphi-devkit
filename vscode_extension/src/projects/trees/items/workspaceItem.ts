import { TreeItem, TreeItemCollapsibleState } from 'vscode';
import { Entities } from '../../entities';
import { ProjectItem } from './project';
import { PROJECTS } from '../../../constants';

export class WorkspaceItem extends TreeItem {
  public projects: ProjectItem[] = [];

  constructor(public readonly workspace: Entities.Workspace) {
    super(workspace.name, TreeItemCollapsibleState.Expanded);
    this.contextValue = PROJECTS.CONTEXT.WORKSPACE;
    for (const link of workspace.project_links) {
      const treeItem = ProjectItem.fromData(link);
      this.projects.push(treeItem);
    }
  }
}
