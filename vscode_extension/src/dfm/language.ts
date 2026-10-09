import { CancellationToken, DefinitionProvider, Location, Position, TextDocument, Uri, window, workspace } from 'vscode';

export class DfmLanguageProvider implements DefinitionProvider {
  async provideDefinition(document: TextDocument, position: Position, token: CancellationToken) {
    const wordRange = document.getWordRangeAtPosition(position);
    if (!wordRange) return;

    const word = document.getText(wordRange);
    if (!/^[_a-zA-Z][_a-zA-Z0-9]*$/.test(word)) return;

    const line = document.lineAt(position.line).text;
    const equalsIndex = line.indexOf('=');
    if (equalsIndex === -1 || position.character <= equalsIndex) return;

    const dfmUri = document.uri;
    const pasPath = dfmUri.fsPath.replace(/\.dfm$/i, '.pas');
    try {
      const pasUri = Uri.file(pasPath);
      const pasDoc = await workspace.openTextDocument(pasUri);
      const text = pasDoc.getText();

      // Look for procedure/function like "procedure TForm1.myOnClick"
      const regex = new RegExp(`(procedure|function)\\s+[^\\.]+\\.${word}`, 'i');
      const match = regex.exec(text);
      if (match) {
        const index = match.index;
        const pos = pasDoc.positionAt(index);
        return new Location(pasUri, pos);
      }
    } catch (err) {
      const error = err as Error;
      window.showErrorMessage(`[Delphi][DfmLanguage] Could not open matching .pas file: ${error.message}`);
    }

    return;
  }
}
