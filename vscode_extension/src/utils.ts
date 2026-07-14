import { basename, dirname, extname } from 'path';
import { Uri, workspace, window } from 'vscode';
import { spawn } from 'child_process';

export async function fileExists(filePath: string | Uri | undefined | null): Promise<boolean> {
  if (!filePath) return false;
  try {
    const uri = filePath instanceof Uri ? filePath : Uri.file(filePath);
    await workspace.fs.stat(uri);
    return true;
  } catch {
    return false;
  }
}

export function basenameNoExt(filePath: string | Uri): string {
  if (filePath instanceof Uri) filePath = filePath.fsPath;

  return basename(filePath, extname(filePath));
}

export function assertError(condition: any, message: string): boolean {
  return !!condition || (window.showErrorMessage(message), false);
}

// Splits a start-parameters string into argv entries, honoring double-quoted segments.
export function splitCommandLineArgs(input: string): string[] {
  const args: string[] = [];
  const regex = /"([^"]*)"|(\S+)/g;
  let match: RegExpExecArray | null;
  while ((match = regex.exec(input)) !== null) args.push(match[1] !== undefined ? match[1] : match[2]);
  return args;
}

// Fuses two start-parameters strings (base first) rather than one replacing the other; blank/absent values contribute nothing.
export function fuseStartParameters(base?: string | null, extra?: string | null): string | undefined {
  const parts = [base, extra].filter((s): s is string => !!s && s.trim().length > 0);
  return parts.length > 0 ? parts.join(' ') : undefined;
}

export function launchExecutable(exePath: string, startParameters?: string | null): void {
  const args = startParameters ? splitCommandLineArgs(startParameters) : [];
  spawn(exePath, args, { cwd: dirname(exePath), detached: true, stdio: 'ignore' }).unref();
}
