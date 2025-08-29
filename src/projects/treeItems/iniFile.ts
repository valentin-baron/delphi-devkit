import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { PROJECTS } from '../../constants';

export class IniFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.IniFile, label, resourceUri);
    this.command = {
      command: PROJECTS.COMMAND.CONFIGURE_OR_CREATE_INI,
      title: 'Open INI File',
      arguments: [this.projectIni]
    };
    this.iconPath = new ThemeIcon('settings');
    this.contextValue = 'iniFile';
  }
}
