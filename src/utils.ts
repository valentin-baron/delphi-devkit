import { dirname, basename, join, extname } from "path/posix";
import { Uri, workspace, window } from "vscode";
import fs from "fs";

export function fileExists(filePath: string | Uri | undefined | null): boolean {
  try {
    return !!filePath && !!(fs.statSync(filePath instanceof Uri ? filePath.fsPath : filePath));
  } catch {
    return false;
  }
}

export function removeBOM(content: string): string {
  if (content.charCodeAt(0) === 0xFEFF) {
    return content.substring(1);
  }
  return content;
}

export async function findIniFromExecutable(executableUri?: string): Promise<Uri | undefined> {
  if (!executableUri) { return undefined; }
  try {
    const executableDir = dirname(executableUri);
    const executableName = basenameNoExt(executableUri);
    const iniPath = join(executableDir, `${executableName}.ini`);
    const ini = Uri.file(iniPath);

    try {
      await workspace.fs.stat(ini);
      return ini;
    } catch {
      return undefined;
    }
  } catch (error) {
    console.error('Failed to find INI from executable:', error);
    return undefined;
  }
}

export function basenameNoExt(filePath: string | Uri): string {
  if (filePath instanceof Uri) {
    filePath = filePath.fsPath;
  }
  return basename(filePath, extname(filePath));
}

function assert(condition: boolean, message: string, callback: (message: string) => any): boolean {
  if (condition) {
    return true;
  }
  callback(message);
  return false;
}

export function assertError(condition: any, message: string): boolean {
  return assert(condition, message, window.showErrorMessage);
}

export function assertWarning(condition: any, message: string): boolean {
  return assert(condition, message, window.showWarningMessage);
}

export function assertInfo(condition: any, message: string): boolean {
  return assert(condition, message, window.showInformationMessage);
}