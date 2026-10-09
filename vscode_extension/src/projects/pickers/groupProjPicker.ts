import { window, workspace, Uri, ProgressLocation } from 'vscode';

export class GroupProjectPicker {
  async pickGroupProject(): Promise<Uri | undefined> {
    const groupProjUris = await window.withProgress(
      {
        location: ProgressLocation.Notification,
        title: 'Scanning for .groupproj files...',
        cancellable: false
      },
      async () => {
        return (await workspace.findFiles('**/*.groupproj')).sort((a, b) => a.fsPath.length - b.fsPath.length);
      }
    );
    if (!groupProjUris.length) {
      window.showInformationMessage('No .groupproj files found in the workspace.');
      return;
    }
    const picked = await window.showQuickPick(
      groupProjUris.map((uri) => ({
        label: workspace.asRelativePath(uri),
        uri
      })),
      { placeHolder: 'Select a .groupproj file to load' }
    );
    return picked?.uri;
  }
}
