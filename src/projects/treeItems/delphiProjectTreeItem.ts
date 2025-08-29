import { TreeItem, TreeItemCollapsibleState, Uri } from 'vscode';
import { DelphiProjectTreeItemType, ProjectType } from '../../types';
import { Entities } from '../../db/entities';
import { PROJECTS } from '../../constants';

export interface DelphiProjectMainTreeItem {
  entity: Entities.Project;
  link: Entities.ProjectLink;
  resourceUri: Uri;
}

export abstract class DelphiProjectTreeItem extends TreeItem {
  public project: DelphiProjectMainTreeItem;

  constructor(
    public readonly itemType: DelphiProjectTreeItemType,
    public readonly label: string,
    public resourceUri: Uri
  ) {
    super(label, itemType === DelphiProjectTreeItemType.Project ? TreeItemCollapsibleState.Collapsed : TreeItemCollapsibleState.None);
    this.contextValue = PROJECTS.CONTEXT.PROJECT_FILE;
    this.tooltip = this.resourceUri.fsPath;
  }

  public get projectUri(): Uri {
    return this.project.resourceUri;
  }

  public get projectSortValue(): string {
    return this.project.link.sortValue;
  }

  public get projectDproj(): Uri | undefined {
    if (this.project.entity.dproj) {
      return Uri.file(this.project.entity.dproj);
    }
  }

  public get projectDpr(): Uri | undefined {
    if (this.project.entity.dpr) {
      return Uri.file(this.project.entity.dpr);
    }
  }

  public get projectDpk(): Uri | undefined {
    if (this.project.entity.dpk) {
      return Uri.file(this.project.entity.dpk);
    }
  }

  public get projectExe(): Uri | undefined {
    if (this.project.entity.exe) {
      return Uri.file(this.project.entity.exe);
    }
  }

  public get projectIni(): Uri | undefined {
    if (this.project.entity.ini) {
      return Uri.file(this.project.entity.ini);
    }
  }

  public get projectType(): ProjectType {
    if (this.projectDpk) {
      return ProjectType.Package;
    }
    return ProjectType.Application;
  }
}
