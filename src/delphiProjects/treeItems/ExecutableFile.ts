import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export class ExecutableFile extends DelphiProjectTreeItem {
  public ini?: Uri;

  constructor(
    label: string,
    resourceUri: Uri,
    collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.None
  ) {
    super(label, resourceUri, collapsibleState);
    this.command = {
      command: 'delphi-utils.launchExecutable',
      title: 'Launch Application',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('run');
    this.contextValue = 'executableFile';
  }
}
