import { Uri, workspace } from "vscode";
import { promises as fs } from "fs";
import { dirname, join, basename } from "path";
import { basenameNoExt, fileExists, removeBOM } from "../../utils";
import { DOMParser } from "@xmldom/xmldom";

export class DprojParser {
  public async findExecutable(dprojUri?: Uri): Promise<Uri | undefined> {
    if (!dprojUri) { return; }
    try {
      let dprojContent = await fs.readFile(dprojUri.fsPath, 'utf8');
      dprojContent = removeBOM(dprojContent);

      // Use faster regex-based parsing instead of full XML parsing for this specific case
      // Look for DCC_DependencyCheckOutputName in PropertyGroup elements
      const outputNameMatch = dprojContent.match(/<DCC_DependencyCheckOutputName[^>]*>([^<]+)<\/DCC_DependencyCheckOutputName>/i);

      if (outputNameMatch && outputNameMatch[1] && outputNameMatch[1].toLowerCase().endsWith('.exe')) {
        const outputPath = outputNameMatch[1].trim();
        if (outputPath) {
          // The path might be relative to the DPROJ location
          const dprojDir = dirname(dprojUri.fsPath);
          const executablePath = join(dprojDir, outputPath);
          if (fileExists(executablePath)) {
            return Uri.file(executablePath);
          }
        }
      }

      // Fallback to full XML parsing if regex approach didn't work
      return await this.findExecutableFromOutputPaths(dprojUri, dprojContent);
    } catch (error) {
      console.error('Failed to parse DPROJ file:', error);
      return;
    }
  }

  private async findExecutableFromOutputPaths(dprojUri: Uri, dprojContent: string): Promise<Uri | undefined> {
    try {
      const parser = new DOMParser();
      const xmlDoc = parser.parseFromString(dprojContent, 'text/xml');

      // Find all PropertyGroup elements
      const propertyGroups = xmlDoc.getElementsByTagName('PropertyGroup');

      for (let i = 0; i < propertyGroups.length; i++) {
        const propertyGroup = propertyGroups[i];
        const dccElements = propertyGroup.getElementsByTagName('DCC_ExeOutput');

        if (dccElements.length > 0) {
          const outputPath = dccElements[0].textContent;
          if (outputPath) {
            // The path might be relative to the DPROJ location
            const dprojDir = dirname(dprojUri.fsPath);
            const executablePath = join(dprojDir, outputPath, basename(dprojUri.fsPath).replace('.dproj', '.exe'));
            if (fileExists(executablePath)) {
              return Uri.file(executablePath);
            }
          }
        }
      }

      return;
    } catch (error) {
      console.error('Failed to parse DPROJ file with XML fallback:', error);
      return;
    }
  }

  public async findDpr(dprojUri: Uri): Promise<Uri | undefined> {
    const dprojDir = workspace.asRelativePath(dirname(dprojUri.fsPath));
    const dprojName = basenameNoExt(dprojUri);

    // Look for a DPR file with the same base name in the same directory
    const dprPattern = join(dprojDir, `${dprojName}.[Dd][Pp][Rr]`);
    const foundFiles = await workspace.findFiles(dprPattern);

    if (foundFiles.length > 0) {
      return foundFiles[0];
    }
  }

  public async findDpk(dprojUri: Uri): Promise<Uri | undefined> {
    const dprojDir = workspace.asRelativePath(dirname(dprojUri.fsPath));
    const dprojName = basenameNoExt(dprojUri.fsPath);

    // Look for a DPR file with the same base name in the same directory
    const dprPattern = join(dprojDir, `${dprojName}.[Dd][Pp][Kk]`);
    const foundFiles = await workspace.findFiles(dprPattern);

    if (foundFiles.length > 0) {
      return foundFiles[0];
    }
  }
}
