import { TreeItem, TreeItemCollapsibleState, Uri } from 'vscode';
import { dirname } from 'path';

export abstract class DelphiProjectTreeItem extends TreeItem {
  constructor(
    public readonly label: string,
    public readonly resourceUri: Uri,
    collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.None
  ) {
    super(label, collapsibleState);
    this.tooltip = this.resourceUri.fsPath;
    this.description = dirname(this.resourceUri.fsPath);
  }
}
