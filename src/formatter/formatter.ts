import {
  DocumentFormattingEditProvider,
  TextDocument,
  TextEdit,
  workspace,
  Range,
  window,
  DocumentRangeFormattingEditProvider
} from 'vscode';
import { assertError } from '../utils';
import { FORMAT } from '../constants';
import { execFile } from 'node:child_process';
import { Runtime } from '../runtime';
import { tmpdir } from 'node:os';
import { readFileSync, unlinkSync, writeFileSync } from 'node:fs';

class BaseFormattingProvider {
  protected execute(
    exe: string,
    file: string,
    args: string[] = []
  ): void {
    try {
      execFile(
        exe,
        [
          '-delphi',
          '-config',
          Runtime.extension.asAbsolutePath('ddk_formatter.config'),
          ...args,
          file
        ]
      );
    } catch (e) {
      window.showWarningMessage(`Format error: ${(e as Error).message}`);
    }
  }
}

export class FullFileFormattingProvider extends BaseFormattingProvider implements DocumentFormattingEditProvider {
  public async provideDocumentFormattingEdits(
    document: TextDocument
  ): Promise<TextEdit[]> {
    const cfg = workspace.getConfiguration(FORMAT.KEY);
    if (!cfg.get<boolean>(FORMAT.CONFIG.ENABLE)) return [];

    const exe = cfg.get<string>(FORMAT.CONFIG.PATH);
    if (!assertError(exe, 'No formatter configured.')) return [];

    const args = cfg.get<string[]>(FORMAT.CONFIG.ARGS) || [];

    if (exe)
      try {
        this.execute(exe, document.fileName, args);
      } catch (e) {
        window.showWarningMessage(`Format error: ${(e as Error).message}`);
      }

    return [];
  }
}

export class RangeFormattingProvider extends BaseFormattingProvider implements DocumentRangeFormattingEditProvider {
  public async provideDocumentRangeFormattingEdits(
    document: TextDocument,
    range: Range
  ): Promise<TextEdit[]> {
    // export full lines content into temp file and format it, read it back and delete temp file.
    const cfg = workspace.getConfiguration(FORMAT.KEY);
    if (!cfg.get<boolean>(FORMAT.CONFIG.ENABLE)) return [];
    const exe = cfg.get<string>(FORMAT.CONFIG.PATH);
    if (!assertError(exe, 'No formatter configured.')) return [];
    const args = cfg.get<string[]>(FORMAT.CONFIG.ARGS) || [];
    if (exe) {
      const tempFile = `${tmpdir()}/ddk_format_${Date.now()}.pas`;
      try {
        const fullLines = new Range(range.start.line, 0, range.end.line, document.lineAt(range.end.line).text.length);
        writeFileSync(tempFile, document.getText(fullLines), { encoding: document.encoding as BufferEncoding || 'utf8' });
        try {
          this.execute(exe, tempFile, args);
          return [
            TextEdit.replace(
              fullLines,
              readFileSync(
                tempFile,
                { encoding: document.encoding as BufferEncoding || 'utf8' }
              )
            )
          ];
        } catch (e) {
          window.showWarningMessage(`Format error: ${(e as Error).message}`);
        }
      } finally {
        unlinkSync(tempFile);
      }
    }
    return [];
  }
}