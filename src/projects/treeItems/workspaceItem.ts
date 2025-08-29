import { TreeItem, TreeItemCollapsibleState } from "vscode";
import { Entities } from "../../db/entities";
import { DelphiProject } from "./delphiProject";
import { PROJECTS } from "../../constants";

export class WorkspaceItem extends TreeItem {
  public projects: DelphiProject[] = [];

  constructor(
    public readonly workspace: Entities.Workspace
  ) {
    super(workspace.name, TreeItemCollapsibleState.Expanded);
    this.contextValue = PROJECTS.CONTEXT.WORKSPACE;
    for (const link of workspace.projects) {
      const treeItem = DelphiProject.fromData(link);
      this.projects.push(treeItem);
    }
    this.projects = this.projects.sort((a, b) => a.sortValue.localeCompare(b.sortValue));
  }
}