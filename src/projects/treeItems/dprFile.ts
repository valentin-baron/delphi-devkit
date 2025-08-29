import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';

export class DprFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri,
  ) {
    super(DelphiProjectTreeItemType.DprFile, label, resourceUri);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPR File',
      arguments: [this.projectDpr]
    };
    this.iconPath = new ThemeIcon('file-code');
    this.contextValue = 'dprFile';
  }
}
