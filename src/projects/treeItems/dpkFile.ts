import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';
import { ProjectType } from './delphiProject';

export class DpkFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.DpkFile, label, resourceUri, ProjectType.Package);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPK File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('package');
    this.contextValue = 'dpkFile';
  }
}
