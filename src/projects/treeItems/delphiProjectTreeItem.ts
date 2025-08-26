import { TreeItem, TreeItemCollapsibleState, Uri } from 'vscode';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export interface DelphiProjectMainTreeItem {
  projectId?: number;
  dproj?: Uri;
  dpr?: Uri;
  dpk?: Uri;
  exe?: Uri;
  ini?: Uri;
  sortValue: string;
  resourceUri: Uri;
}

export abstract class DelphiProjectTreeItem extends TreeItem {
  public project: DelphiProjectMainTreeItem;

  constructor(
    public readonly itemType: DelphiProjectTreeItemType,
    public readonly label: string,
    public readonly resourceUri: Uri,
    public readonly projectType: ProjectType
  ) {
    super(label, itemType === DelphiProjectTreeItemType.Project ? TreeItemCollapsibleState.Collapsed : TreeItemCollapsibleState.None);
    this.tooltip = this.resourceUri.fsPath;
  }

  public get projectUri(): Uri {
    return this.project.resourceUri;
  }

  public get projectSortValue(): string {
    return this.project.sortValue;
  }

  public get projectDproj(): Uri | undefined {
    return this.project.dproj;
  }

  public get projectDpr(): Uri | undefined {
    return this.project.dpr;
  }

  public get projectDpk(): Uri | undefined {
    return this.project.dpk;
  }

  public get projectExe(): Uri | undefined {
    return this.project.exe;
  }

  public get projectIni(): Uri | undefined {
    return this.project.ini;
  }
}
