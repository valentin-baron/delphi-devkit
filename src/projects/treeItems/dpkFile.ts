import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';

export class DpkFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.DpkFile, label, resourceUri);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPK File',
      arguments: [this.projectDpk]
    };
    this.iconPath = new ThemeIcon('package');
    this.contextValue = 'dpkFile';
  }
}
