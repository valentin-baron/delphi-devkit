import { Uri } from 'vscode';
import { promises as fs } from 'fs';

/**
 * Utility to parse .groupproj files and extract dproj paths.
 */
export class GroupProjParser {
  /**
   * Parse a .groupproj file and return a list of dproj paths (relative to workspace root).
   */
  static async parseGroupProjFile(groupProjUri: Uri): Promise<{ name: string; groupProjPath: string; groupProjAbsolutePath: string; dprojPaths: string[] }> {
    const content = await fs.readFile(groupProjUri.fsPath, 'utf8');
    // Simple regex to extract all <Projects Include="..."> tags
    const projectRegex = /<Projects\s+Include="([^"]+)"/gi;
    const dprojPaths: string[] = [];
    let match;
    while ((match = projectRegex.exec(content))) {
      const relPath = match[1];
      if (relPath.toLowerCase().endsWith('.dproj')) {
        dprojPaths.push(relPath);
      }
    }
    return {
      name: groupProjUri.fsPath.split(/[\\\/]/).pop() || groupProjUri.fsPath,
      groupProjPath: groupProjUri.fsPath,
      groupProjAbsolutePath: groupProjUri.fsPath,
      dprojPaths
    };
  }
}
