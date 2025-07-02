import * as vscode from 'vscode';
import * as path from 'path';

export async function dfmSwap(): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    vscode.window.showInformationMessage('No editor is active.');
    return;
  }

  const currentUri = editor.document.uri;
  const currentExt = path.extname(currentUri.fsPath).toLowerCase();

  if (currentExt !== '.pas' && currentExt !== '.dfm') {
    vscode.window.showInformationMessage('Not a .pas or .dfm file.');
    return;
  }

  const targetExt = currentExt === '.pas' ? '.dfm' : '.pas';
  const targetPath = currentUri.fsPath.replace(/\.pas$|\.dfm$/i, targetExt);
  const targetUri = vscode.Uri.file(targetPath);

  const openEditors = vscode.window.visibleTextEditors;
  const alreadyOpen = openEditors.find(openEditor =>
    openEditor.document.uri.fsPath.toLowerCase() === targetPath.toLowerCase()
  );

  if (alreadyOpen) {
    await vscode.window.showTextDocument(alreadyOpen.document, alreadyOpen.viewColumn);
    return;
  }

  try {
    const doc = await vscode.workspace.openTextDocument(targetUri);
    await vscode.window.showTextDocument(doc, editor.viewColumn);
  } catch (err) {
    const error = err as Error;
    vscode.window.showErrorMessage(`Cannot open ${targetExt.toUpperCase()} file: ${error.message}`);
  }
};
