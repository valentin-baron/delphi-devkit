import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export class DprojFile extends DelphiProjectTreeItem {
  public dpr?: Uri;
  public dpk?: Uri;
  public executable?: Uri;
  public ini?: Uri;

  constructor(
    label: string,
    resourceUri: Uri,
    collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.None
  ) {
    super(label, resourceUri, collapsibleState);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPROJ File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('gear');
    this.contextValue = 'dprojFile';
  }
}
