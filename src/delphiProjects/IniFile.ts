import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';

export class IniFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri
  ) {
    super(label, resourceUri, TreeItemCollapsibleState.None);
    this.command = {
      command: 'vscode.open',
      title: 'Open INI File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('settings');
    this.contextValue = 'iniFile';
  }
}
