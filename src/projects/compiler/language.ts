import { CancellationToken, DocumentLink, DocumentLinkProvider, TextDocument, Range, Position, Uri, workspace, DiagnosticCollection, languages, Diagnostic, DiagnosticSeverity, DiagnosticTag } from "vscode";
import { fileExists } from "../../utils";
import { PROJECTS } from "../../constants";
import { Runtime } from "../../runtime";

export namespace CompilerOutputLanguage {
  //   1_______    2____  3_____  4_________________        5_   5_______________________
  // ( 19:07:48 ) [ERROR] [E1234] C:\Path\To\File.pas (line 42): Description of the error
  export const PATTERN = /^\( ([^)]+?) \) \[([^\]]+)\] \[([^\]]+)\] (.*?):(\d+) - (.*)$/;
  export const CONTENT = 0;
  export const TIME = 1;
  export const SEVERITY = 2;
  export const CODE = 3;
  export const FILE = 4;
  export const LINE = 5;
  export const MESSAGE = 6;

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
    const dotIndex = quotedContent.indexOf('.'); // if the quoted content is referencing Class.Member, slice to just Member
    const contentToFind = (dotIndex > 0 ? quotedContent.slice(dotIndex + 1) : quotedContent).toLowerCase();
    const targetLine = lineText.toLowerCase();
    if (contentToFind.length > 0 && targetLine.indexOf(contentToFind) >= 0)
      return Math.max(targetLine.indexOf(contentToFind) + 1, 1);
  }
  return 1; // Default to column 1 if nothing found
}

export class CompilerOutputDefinitionProvider implements DocumentLinkProvider {
  public compilerIsActive: boolean = false;

  constructor(
    private readonly diagnosticCollection: DiagnosticCollection = languages.createDiagnosticCollection(PROJECTS.LANGUAGES.COMPILER)
  ) {
    Runtime.extension.subscriptions.push(this.diagnosticCollection);
  }

  // Called by outputChannel.Show()
  public async provideDocumentLinks(
    document: TextDocument,
    token: CancellationToken
  ): Promise<DocumentLink[]> {
    if (this.compilerIsActive) return []; // Don't provide links while compiler is running
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

    this.diagnosticCollection.clear();

    return (await Promise.all(
      matchesByFile.map(async (o) => {
        const fileName = o.file;
        if (token.isCancellationRequested) throw new Error('Operation cancelled');
        if (!fileExists(fileName)) return [];
        const fileContent = await workspace.fs.readFile(Uri.file(fileName));
        const fileText = Buffer.from(fileContent).toString('utf8');
        const fileLines = fileText.split(/\r?\n/g);
        const diagnostics: Diagnostic[] = [];
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

          const severity = DIAGNOSTIC_SEVERITY[match[CompilerOutputLanguage.SEVERITY].toUpperCase() as keyof typeof DIAGNOSTIC_SEVERITY];
          const diagnostic = new Diagnostic(codeLink.range, message, severity);
          diagnostic.code = code;
          diagnostic.source = 'MSBuild (DDK)';
          if (diagnostic.code === 'W1000') diagnostic.tags = [DiagnosticTag.Deprecated];
          diagnostics.push(diagnostic);

          return [fileLink, codeLink];
        });
        this.diagnosticCollection.set(Uri.file(fileName), diagnostics);
        return links;
      })
    )).flat(2);
  }
  public resolveDocumentLink(
    link: DocumentLink,
    token: CancellationToken
  ): undefined {} // Dont do anything with incomplete links
}