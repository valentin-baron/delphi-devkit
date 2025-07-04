import { Uri, workspace } from 'vscode';
import { basename, dirname, join } from 'path';
import { promises as fs } from 'fs';
import { DOMParser } from '@xmldom/xmldom';

/**
 * Utility functions for Delphi Projects operations
 */
export class DelphiProjectUtils {

  /**
   * Remove BOM (Byte Order Mark) from file content if present
   * This is necessary for proper XML parsing of DPROJ files created by Windows tools
   */
  private static removeBOM(content: string): string {
    if (content.charCodeAt(0) === 0xFEFF) {
      return content.substring(1);
    }
    return content;
  }

  /**
   * Find the executable path from a DPROJ file by parsing its XML content (optimized version)
   */
  static async findExecutableFromDproj(dprojUri: Uri): Promise<Uri | null> {
    try {
      let dprojContent = await fs.readFile(dprojUri.fsPath, 'utf8');
      dprojContent = this.removeBOM(dprojContent);

      // Use faster regex-based parsing instead of full XML parsing for this specific case
      // Look for DCC_DependencyCheckOutputName in PropertyGroup elements
      const outputNameMatch = dprojContent.match(/<DCC_DependencyCheckOutputName[^>]*>([^<]+)<\/DCC_DependencyCheckOutputName>/i);

      if (outputNameMatch && outputNameMatch[1]) {
        const outputPath = outputNameMatch[1].trim();
        if (outputPath) {
          // The path might be relative to the DPROJ location
          const dprojDir = dirname(dprojUri.fsPath);
          const executablePath = join(dprojDir, outputPath);
          return Uri.file(executablePath);
        }
      }

      // Fallback to full XML parsing if regex approach didn't work
      return await this.findExecutableFromDprojFallback(dprojUri, dprojContent);
    } catch (error) {
      console.error('Failed to parse DPROJ file:', error);
      return null;
    }
  }

  /**
   * Fallback method using full XML parsing when regex approach fails
   */
  private static async findExecutableFromDprojFallback(dprojUri: Uri, dprojContent: string): Promise<Uri | null> {
    try {
      const parser = new DOMParser();
      const xmlDoc = parser.parseFromString(dprojContent, 'text/xml');

      // Find all PropertyGroup elements
      const propertyGroups = xmlDoc.getElementsByTagName('PropertyGroup');

      for (let i = 0; i < propertyGroups.length; i++) {
        const propertyGroup = propertyGroups[i];
        const dccElements = propertyGroup.getElementsByTagName('DCC_DependencyCheckOutputName');

        if (dccElements.length > 0) {
          const outputPath = dccElements[0].textContent;
          if (outputPath) {
            // The path might be relative to the DPROJ location
            const dprojDir = dirname(dprojUri.fsPath);
            const executablePath = join(dprojDir, outputPath);
            return Uri.file(executablePath);
          }
        }
      }

      return null;
    } catch (error) {
      console.error('Failed to parse DPROJ file with XML fallback:', error);
      return null;
    }
  }

  /**
   * Find the associated DPR file for a given DPROJ file
   */
  static async findDprFromDproj(dprojUri: Uri): Promise<Uri | null> {
    try {
      const dprojDir = dirname(dprojUri.fsPath);
      const dprojName = basename(dprojUri.fsPath).replace(/\.[^/.]+$/, "");

      // Look for a DPR file with the same base name in the same directory
      const dprPath = join(dprojDir, `${dprojName}.dpr`);

      try {
        await workspace.fs.stat(Uri.file(dprPath));
        return Uri.file(dprPath);
      } catch {
        // Try case variations
        const dprPathUpper = join(dprojDir, `${dprojName}.DPR`);
        try {
          await workspace.fs.stat(Uri.file(dprPathUpper));
          return Uri.file(dprPathUpper);
        } catch {
          return null;
        }
      }
    } catch (error) {
      console.error('Failed to find DPR from DPROJ:', error);
      return null;
    }
  }

  /**
   * Find the associated DPROJ file for a given DPR file
   */
  static async findDprojFromDpr(dprUri: Uri): Promise<Uri | null> {
    try {
      const dprDir = dirname(dprUri.fsPath);
      const dprName = basename(dprUri.fsPath).replace(/\.[^/.]+$/, "");

      // Look for a DPROJ file with the same base name in the same directory
      const dprojPath = join(dprDir, `${dprName}.dproj`);

      try {
        await workspace.fs.stat(Uri.file(dprojPath));
        return Uri.file(dprojPath);
      } catch {
        // Try case variations
        const dprojPathUpper = join(dprDir, `${dprName}.DPROJ`);
        try {
          await workspace.fs.stat(Uri.file(dprojPathUpper));
          return Uri.file(dprojPathUpper);
        } catch {
          return null;
        }
      }
    } catch (error) {
      console.error('Failed to find DPROJ from DPR:', error);
      return null;
    }
  }

  /**
   * Find the project files (DPR and DPROJ) associated with an executable
   */
  static async findProjectFromExecutable(executableUri: Uri): Promise<{ dpr?: Uri; dproj?: Uri }> {
    try {
      const execDir = dirname(executableUri.fsPath);
      const execName = basename(executableUri.fsPath).replace(/\.[^/.]+$/, "");

      const result: { dpr?: Uri; dproj?: Uri } = {};

      // Look for DPR file
      const dprPath = join(execDir, `${execName}.dpr`);
      try {
        await workspace.fs.stat(Uri.file(dprPath));
        result.dpr = Uri.file(dprPath);
      } catch {
        // Try case variations
        const dprPathUpper = join(execDir, `${execName}.DPR`);
        try {
          await workspace.fs.stat(Uri.file(dprPathUpper));
          result.dpr = Uri.file(dprPathUpper);
        } catch {
          // DPR not found
        }
      }

      // Look for DPROJ file
      const dprojPath = join(execDir, `${execName}.dproj`);
      try {
        await workspace.fs.stat(Uri.file(dprojPath));
        result.dproj = Uri.file(dprojPath);
      } catch {
        // Try case variations
        const dprojPathUpper = join(execDir, `${execName}.DPROJ`);
        try {
          await workspace.fs.stat(Uri.file(dprojPathUpper));
          result.dproj = Uri.file(dprojPathUpper);
        } catch {
          // DPROJ not found
        }
      }

      return result;
    } catch (error) {
      console.error('Failed to find project from executable:', error);
      return {};
    }
  }

  /**
   * Find the associated DPROJ file for a given DPK file
   */
  static async findDprojFromDpk(dpkUri: Uri): Promise<Uri | null> {
    try {
      const dpkDir = dirname(dpkUri.fsPath);
      const dpkName = basename(dpkUri.fsPath).replace(/\.[^/.]+$/, "");

      // Look for a DPROJ file with the same name in the same directory
      const dprojPath = join(dpkDir, `${dpkName}.dproj`);

      try {
        await workspace.fs.stat(Uri.file(dprojPath));
        return Uri.file(dprojPath);
      } catch {
        // Try case variations
        const dprojPathUpper = join(dpkDir, `${dpkName}.DPROJ`);
        try {
          await workspace.fs.stat(Uri.file(dprojPathUpper));
          return Uri.file(dprojPathUpper);
        } catch {
          // DPROJ not found in same directory with same name
          return null;
        }
      }
    } catch (error) {
      console.error('Failed to find DPROJ from DPK:', error);
      return null;
    }
  }

  /**
   * Find the associated DPK file for a given DPROJ file
   */
  static async findDpkFromDproj(dprojUri: Uri): Promise<Uri | null> {
    try {
      const dprojDir = dirname(dprojUri.fsPath);
      const dprojName = basename(dprojUri.fsPath).replace(/\.[^/.]+$/, "");

      // Look for a DPK file with the same base name in the same directory
      const dpkPath = join(dprojDir, `${dprojName}.dpk`);

      try {
        await workspace.fs.stat(Uri.file(dpkPath));
        return Uri.file(dpkPath);
      } catch {
        // Try case variations
        const dpkPathUpper = join(dprojDir, `${dprojName}.DPK`);
        try {
          await workspace.fs.stat(Uri.file(dpkPathUpper));
          return Uri.file(dpkPathUpper);
        } catch {
          return null;
        }
      }
    } catch (error) {
      console.error('Failed to find DPK from DPROJ:', error);
      return null;
    }
  }
}
