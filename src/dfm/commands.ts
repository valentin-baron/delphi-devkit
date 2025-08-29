import { window, Uri, workspace, Disposable, commands } from 'vscode';
import { extname } from 'path';
import { DFM } from '../constants';

export class DfmCommands {
  public static get registers(): Disposable[] {
    return [
      commands.registerCommand(DFM.Commands.SWAP_DFM_PAS, this.dfmSwap.bind(this))
    ];
  }
  private static async dfmSwap(): Promise<void> {
    const editor = window.activeTextEditor;
    if (!editor) {
      window.showInformationMessage('No editor is active.');
      return;
    }

    const currentUri = editor.document.uri;
    const currentExt = extname(currentUri.fsPath).toLowerCase();

    if (currentExt !== '.pas' && currentExt !== '.dfm') {
      window.showInformationMessage('Not a .pas or .dfm file.');
      return;
    }

    const targetExt = currentExt === '.pas' ? '.dfm' : '.pas';
    const targetPath = currentUri.fsPath.replace(/\.pas$|\.dfm$/i, targetExt);
    const targetUri = Uri.file(targetPath);

    const openEditors = window.visibleTextEditors;
    const alreadyOpen = openEditors.find(openEditor =>
      openEditor.document.uri.fsPath.toLowerCase() === targetPath.toLowerCase()
    );

    if (alreadyOpen) {
      await window.showTextDocument(alreadyOpen.document, alreadyOpen.viewColumn);
      return;
    }

    try {
      const doc = await workspace.openTextDocument(targetUri);
      await window.showTextDocument(doc, editor.viewColumn);
    } catch (err) {
      const error = err as Error;
      window.showErrorMessage(`Cannot open ${targetExt.toUpperCase()} file: ${error.message}`);
    }
  }
}
