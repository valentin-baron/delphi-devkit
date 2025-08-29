import { ThemeIcon, Uri } from 'vscode';
import { DelphiProjectTreeItem } from './delphiProjectTreeItem';
import { DelphiProjectTreeItemType } from '../../types';

export class DprojFile extends DelphiProjectTreeItem {
  constructor(
    label: string,
    resourceUri: Uri
  ) {
    super(DelphiProjectTreeItemType.DprojFile, label, resourceUri);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPROJ File',
      arguments: [this.projectDproj]
    };
    this.iconPath = new ThemeIcon('gear');
    this.contextValue = 'dprojFile';
  }
}
