import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export class DprojFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
    projectType: ProjectType,
  ) {
    super(DelphiProjectTreeItemType.DprojFile, label, resourceUri, projectType);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPROJ File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('gear');
    this.contextValue = 'dprojFile';
  }
}
