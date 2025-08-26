import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export class DprFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.DprFile, label, resourceUri, ProjectType.Application);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPR File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('file-code');
    this.contextValue = 'dprFile';
  }
}
