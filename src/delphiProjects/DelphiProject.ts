import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export enum ProjectType {
  Application = 'application',
  Package = 'package',
  Library = 'library'
}

export class DelphiProject extends DelphiProjectTreeItem {
  public dproj?: Uri;
  public dpr?: Uri;
  public dpk?: Uri;
  public executable?: Uri;
  public ini?: Uri;
  public projectType: ProjectType;

  constructor(
    label: string,
    projectType: ProjectType,
    collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.Collapsed
  ) {
    // For project items, we use a dummy URI initially, will be updated when files are set
    super(label, Uri.parse('delphi-project:' + label), collapsibleState);
    this.projectType = projectType;
    this.contextValue = 'delphiProject';

    // Set icon based on project type
    switch (projectType) {
      case ProjectType.Package:
        this.iconPath = new ThemeIcon('package');
        break;
      case ProjectType.Library:
        this.iconPath = new ThemeIcon('library');
        break;
      case ProjectType.Application:
      default:
        this.iconPath = new ThemeIcon('symbol-class');
        break;
    }
  }

  // Get the most appropriate resource URI for commands
  getResourceUri(): Uri {
    if (this.dproj) {
      return this.dproj;
    }
    if (this.dpr) {
      return this.dpr;
    }
    if (this.dpk) {
      return this.dpk;
    }
    return this.resourceUri;
  }

  // Update collapsible state based on children
  updateCollapsibleState(): void {
    const hasChildren = !!(this.dproj || this.dpr || this.dpk || this.executable);
    this.collapsibleState = hasChildren ? TreeItemCollapsibleState.Collapsed : TreeItemCollapsibleState.None;
  }
}
