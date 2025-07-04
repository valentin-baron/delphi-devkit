import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export class DprFile extends DelphiProjectTreeItem {
  public dproj?: Uri;
  public executable?: Uri;

  constructor(
    label: string,
    resourceUri: Uri,
    collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.None
  ) {
    super(label, resourceUri, collapsibleState);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPR File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('file-code');
    this.contextValue = 'dprFile';
  }
}
