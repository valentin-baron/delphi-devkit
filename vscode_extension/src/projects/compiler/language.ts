import { CancellationToken, DocumentLink, DocumentLinkProvider, TextDocument, Range, Position, Uri, workspace, DiagnosticSeverity } from "vscode";
import { fileExists } from "../../utils";

export namespace CompilerOutputLanguage {
  //   1________  2_____  3_____  4________________________  5_  6_ (opt)
  // 19:07:48.123: [ERROR][E1234] C:\Path\To\File.pas:42(:5)? - Description of the error
  export const PATTERN = /^(\S+?):\s+\[([^\]]+)\]\[([^\]]+)\]\s+(.*?):(\d+)(?::(\d+))?\s+-\s+(.*)$/;
  export const CONTENT = 0;
  export const TIME = 1;
  export const SEVERITY = 2;
  export const CODE = 3;
  export const FILE = 4;
  export const LINE = 5;
  export const COLUMN = 6;
  export const MESSAGE = 7;

  export const CODE_URL = 'https://docwiki.embarcadero.com/RADStudio/index.php?search=Delphi+';
}

const DIAGNOSTIC_SEVERITY = {
  HINT: DiagnosticSeverity.Hint,
  WARN: DiagnosticSeverity.Warning,
  ERROR: DiagnosticSeverity.Error
};

export function getColumnInLine(lineText: string, message: string): number {
  const quotedString = message.match(/'(.*?)'/); // '%s' usually points to some symbol
  if (quotedString) {
    const quotedContent = quotedString ? quotedString[1] : '';
    const dotIndex = quotedContent.indexOf('.'); // a quoted Class.Member is searched for as Member
    const contentToFind = (dotIndex > 0 ? quotedContent.slice(dotIndex + 1) : quotedContent).toLowerCase();
    const targetLine = lineText.toLowerCase();
    if (contentToFind.length > 0 && targetLine.indexOf(contentToFind) >= 0)
      return Math.max(targetLine.indexOf(contentToFind) + 1, 1);
  }
  return 1;
}

export class CompilerOutputDefinitionProvider implements DocumentLinkProvider {
  public compilerIsActive: boolean = false;

  // Called by outputChannel.Show()
  public async provideDocumentLinks(
    document: TextDocument,
    token: CancellationToken
  ): Promise<DocumentLink[]> {
    if (this.compilerIsActive) return [];
    const text = document.getText();
    let lines = text.split(/\r?\n/g);
    const matches = (
      await Promise.all(
        lines.map(line => line.match(CompilerOutputLanguage.PATTERN))
      )
    ).filter((match) => !!match);

    const matchesByFile = matches.reduce((acc, match) => {
      if (match) {
        const file = match[CompilerOutputLanguage.FILE];
        const existing = acc.find(item => item.file === file);
        if (existing) existing.matches.push(match);
        else acc.push({ file, matches: [match] });
      }
      return acc;
    }, [] as { file: string, matches: RegExpMatchArray[] }[]);

    return (await Promise.all(
      matchesByFile.map(async (o) => {
        const fileName = o.file;
        if (token.isCancellationRequested) throw new Error('Operation cancelled');
        if (!await fileExists(fileName)) return [];
        const fileContent = await workspace.fs.readFile(Uri.file(fileName));
        const fileText = Buffer.from(fileContent).toString('utf8');
        const fileLines = fileText.split(/\r?\n/g);
        const links = o.matches.map((match) => {
          const line = match[0];
          const lineIndex = lines.indexOf(line);
          const code = match[CompilerOutputLanguage.CODE];
          const file = match[CompilerOutputLanguage.FILE];
          const lineNumText = match[CompilerOutputLanguage.LINE];
          const lineNum = parseInt(lineNumText, 10);
          const message = match[CompilerOutputLanguage.MESSAGE];
          const codeIndex = line.indexOf(code);
          const fileIndex = line.indexOf(file);
          const column = getColumnInLine(fileLines[lineNum - 1] || '', message);

          const codeLink = new DocumentLink(
            new Range(
              new Position(lineIndex, codeIndex),
              new Position(lineIndex, codeIndex + code.length)),
            Uri.parse(`${CompilerOutputLanguage.CODE_URL}${code}`)
          );
          const fileLink = new DocumentLink(
            new Range(
              new Position(lineIndex, fileIndex),
              new Position(lineIndex, fileIndex + file.length + lineNumText.length + 1)),
            Uri.file(file).with({ fragment: `L${lineNum},${column}` })
          );
          return [fileLink, codeLink];
        });
        return links;
      })
    )).flat(2);
  }
  public resolveDocumentLink(
    link: DocumentLink,
    token: CancellationToken
  ): undefined {} // Links are returned with their target set, so nothing resolves here.
}