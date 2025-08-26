import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export class ExeFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.ExecutableFile, label, resourceUri, ProjectType.Application);
    this.command = {
      command: 'delphi-devkit.projects.runExecutable',
      title: 'Launch Application',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('run');
    this.contextValue = 'executableFile';
  }
}
