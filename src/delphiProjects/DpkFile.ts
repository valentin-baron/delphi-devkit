import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export class DpkFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri
  ) {
    super(label, resourceUri, TreeItemCollapsibleState.None);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPK File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('package');
    this.contextValue = 'dpkFile';
  }
}
