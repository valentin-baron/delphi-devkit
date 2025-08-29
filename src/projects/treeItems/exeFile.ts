import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { PROJECTS } from '../../constants';

export class ExeFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.ExecutableFile, label, resourceUri);
    this.command = {
      command: PROJECTS.COMMAND.RUN_EXECUTABLE,
      title: 'Launch Application',
      arguments: [this.projectExe]
    };
    this.iconPath = new ThemeIcon('run');
    this.contextValue = 'executableFile';
  }
}
