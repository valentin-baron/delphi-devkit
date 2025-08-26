import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export class IniFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.IniFile, label, resourceUri, ProjectType.Application);
    this.command = {
      command: 'vscode.open',
      title: 'Open INI File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('settings');
    this.contextValue = 'iniFile';
  }
}
