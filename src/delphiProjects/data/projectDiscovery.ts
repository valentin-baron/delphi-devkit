import { Uri, workspace, RelativePattern } from 'vscode';
import { basename, dirname, join } from 'path';
import { promises as fs } from 'fs';
import { DelphiProject, ProjectType } from '../treeItems/DelphiProject';
import { DelphiProjectUtils } from '../utils';

/**
 * Project discovery service for finding Delphi projects in the workspace.
 */
export class ProjectDiscovery {

  /**
   * Discover all Delphi projects in the workspace based on configuration.
   */
  static async getAllProjects(): Promise<DelphiProject[]> {
    console.log('ProjectDiscovery: Starting getAllProjects...');

    if (!workspace.workspaceFolders) {
      console.log('ProjectDiscovery: No workspace folders found');
      return [];
    }

    console.log(`ProjectDiscovery: Found ${workspace.workspaceFolders.length} workspace folders`);

    const projectMap = new Map<string, DelphiProject>(); // Key: project base name + directory

    // Get configuration
    const config = workspace.getConfiguration('delphi-utils.delphiProjects');
    const projectPaths: string[] = config.get('projectPaths', ['**']);
    const excludePatterns: string[] = config.get('excludePatterns', []);

    console.log('ProjectDiscovery: Project paths:', projectPaths);
    console.log('ProjectDiscovery: Exclude patterns:', excludePatterns);

    for (const folder of workspace.workspaceFolders) {
      console.log(`ProjectDiscovery: Processing folder: ${folder.uri.fsPath}`);

      // Create exclude pattern for workspace.findFiles
      const excludeGlob = excludePatterns.length > 0 ? `{${excludePatterns.join(',')}}` : undefined;
      console.log('ProjectDiscovery: Using exclude glob:', excludeGlob);

      // Use optimized batch processing approach
      await this.processAllProjectFilesBatch(folder, projectPaths, excludeGlob, projectMap);
    }

    console.log(`ProjectDiscovery: Finished processing, found ${projectMap.size} total projects`);
    return Array.from(projectMap.values());
  }

  /**
   * Optimized batch processing of all project files at once.
   */
  private static async processAllProjectFilesBatch(
    folder: any,
    projectPaths: string[],
    excludeGlob: string | undefined,
    projectMap: Map<string, DelphiProject>
  ): Promise<void> {
    const startTime = Date.now();

    // Build consolidated patterns for all file types
    const dprojPatterns: string[] = [];
    const dprPatterns: string[] = [];
    const dpkPatterns: string[] = [];

    for (const projectPath of projectPaths) {
      dprojPatterns.push(`${projectPath}/**/*.[Dd][Pp][Rr][Oo][Jj]`);
      dprPatterns.push(`${projectPath}/**/*.[Dd][Pp][Rr]`);
      dpkPatterns.push(`${projectPath}/**/*.[Dd][Pp][Kk]`);
    }

    console.log('ProjectDiscovery: Finding all project files in parallel...');

    // Find all files in parallel
    const [dprojFiles, dprFiles, dpkFiles] = await Promise.all([
      this.findFilesBatch(folder, dprojPatterns, excludeGlob),
      this.findFilesBatch(folder, dprPatterns, excludeGlob),
      this.findFilesBatch(folder, dpkPatterns, excludeGlob)
    ]);

    console.log(`ProjectDiscovery: Found ${dprojFiles.length} DPROJ, ${dprFiles.length} DPR, ${dpkFiles.length} DPK files in ${Date.now() - startTime}ms`);

    // Create lookup maps for faster file association
    const filesByDir = this.createFilesByDirectoryMap(dprFiles, dpkFiles);

    // Process DPROJ files first (they take precedence)
    await this.processDprojFilesBatch(dprojFiles, filesByDir, projectMap);

    // Process standalone DPR and DPK files
    this.processStandaloneFilesBatch(dprFiles, dpkFiles, projectMap);

    console.log(`ProjectDiscovery: Batch processing completed in ${Date.now() - startTime}ms`);
  }

  /**
   * Find files using batch patterns to reduce the number of workspace.findFiles calls.
   */
  private static async findFilesBatch(
    folder: any,
    patterns: string[],
    excludeGlob: string | undefined
  ): Promise<Uri[]> {
    if (patterns.length === 0) {
      return [];
    }

    // Use a single combined pattern instead of multiple calls
    const combinedPattern = patterns.length === 1 ? patterns[0] : `{${patterns.join(',')}}`;
    const relativePattern = new RelativePattern(folder, combinedPattern);

    return await workspace.findFiles(relativePattern, excludeGlob);
  }

  /**
   * Create lookup maps for files organized by directory for faster association.
   */
  private static createFilesByDirectoryMap(dprFiles: Uri[], dpkFiles: Uri[]): Map<string, { dpr?: Uri; dpk?: Uri; baseName: string }[]> {
    const filesByDir = new Map<string, { dpr?: Uri; dpk?: Uri; baseName: string }[]>();

    // Process DPR files
    for (const dprFile of dprFiles) {
      const dirPath = dirname(dprFile.fsPath);
      const baseName = basename(dprFile.fsPath).replace(/\.[^/.]+$/, "");

      if (!filesByDir.has(dirPath)) {
        filesByDir.set(dirPath, []);
      }

      const dirFiles = filesByDir.get(dirPath)!;
      let fileEntry = dirFiles.find(f => f.baseName === baseName);
      if (!fileEntry) {
        fileEntry = { baseName };
        dirFiles.push(fileEntry);
      }
      fileEntry.dpr = dprFile;
    }

    // Process DPK files
    for (const dpkFile of dpkFiles) {
      const dirPath = dirname(dpkFile.fsPath);
      const baseName = basename(dpkFile.fsPath).replace(/\.[^/.]+$/, "");

      if (!filesByDir.has(dirPath)) {
        filesByDir.set(dirPath, []);
      }

      const dirFiles = filesByDir.get(dirPath)!;
      let fileEntry = dirFiles.find(f => f.baseName === baseName);
      if (!fileEntry) {
        fileEntry = { baseName };
        dirFiles.push(fileEntry);
      }
      fileEntry.dpk = dpkFile;
    }

    return filesByDir;
  }

  /**
   * Process DPROJ files in batch with optimized executable parsing.
   */
  private static async processDprojFilesBatch(
    dprojFiles: Uri[],
    filesByDir: Map<string, { dpr?: Uri; dpk?: Uri; baseName: string }[]>,
    projectMap: Map<string, DelphiProject>
  ): Promise<void> {
    console.log(`ProjectDiscovery: Processing ${dprojFiles.length} DPROJ files in batch...`);

    // Process DPROJ files with parallel executable parsing
    const dprojPromises = dprojFiles.map(async (dprojFile) => {
      const fileName = basename(dprojFile.fsPath);
      const baseName = fileName.replace(/\.[^/.]+$/, "");
      const dirPath = dirname(dprojFile.fsPath);
      const projectKey = `${baseName}-${dirPath}`;

      // Determine project type by checking for corresponding files
      let projectType = ProjectType.Application;
      const dirFiles = filesByDir.get(dirPath);
      const correspondingFiles = dirFiles?.find(f => f.baseName === baseName);

      if (correspondingFiles?.dpk) {
        projectType = ProjectType.Package;
      }

      const project = new DelphiProject(baseName, projectType);
      project.dproj = dprojFile;

      // Add corresponding files if found
      if (correspondingFiles?.dpk) {
        project.dpk = correspondingFiles.dpk;
      }
      if (correspondingFiles?.dpr) {
        project.dpr = correspondingFiles.dpr;
      }

      // Parse executable asynchronously (but don't await here for parallel processing)
      return this.processExecutableFromDprojAsync(project, dprojFile, baseName).then(() => {
        project.updateCollapsibleState();
        return { projectKey, project };
      });
    });

    // Wait for all DPROJ processing to complete
    const results = await Promise.allSettled(dprojPromises);

    // Add successful results to project map
    results.forEach((result) => {
      if (result.status === 'fulfilled') {
        const { projectKey, project } = result.value;
        projectMap.set(projectKey, project);
        console.log(`ProjectDiscovery: Added DPROJ project: ${project.label}`);
      } else {
        console.error('ProjectDiscovery: Failed to process DPROJ:', result.reason);
      }
    });
  }

  /**
   * Process standalone DPR and DPK files that don't have corresponding DPROJ files.
   */
  private static processStandaloneFilesBatch(
    dprFiles: Uri[],
    dpkFiles: Uri[],
    projectMap: Map<string, DelphiProject>
  ): void {
    console.log(`ProjectDiscovery: Processing standalone files: ${dprFiles.length} DPR, ${dpkFiles.length} DPK`);

    // Process standalone DPR files
    for (const dprFile of dprFiles) {
      const fileName = basename(dprFile.fsPath);
      const baseName = fileName.replace(/\.[^/.]+$/, "");
      const dirPath = dirname(dprFile.fsPath);
      const projectKey = `${baseName}-${dirPath}`;

      // Only add if we don't already have a project with DPROJ
      if (!projectMap.has(projectKey)) {
        const project = new DelphiProject(baseName, ProjectType.Application);
        project.dpr = dprFile;
        project.updateCollapsibleState();
        projectMap.set(projectKey, project);
      }
    }

    // Process standalone DPK files
    for (const dpkFile of dpkFiles) {
      const fileName = basename(dpkFile.fsPath);
      const baseName = fileName.replace(/\.[^/.]+$/, "");
      const dirPath = dirname(dpkFile.fsPath);
      const projectKey = `${baseName}-${dirPath}`;

      // Only add if we don't already have a project
      if (!projectMap.has(projectKey)) {
        const project = new DelphiProject(baseName, ProjectType.Package);
        project.dpk = dpkFile;
        project.updateCollapsibleState();
        projectMap.set(projectKey, project);
      }
    }
  }

  /**
   * Optimized async version of executable processing that can run in parallel.
   */
  private static async processExecutableFromDprojAsync(
    project: DelphiProject,
    dprojFile: Uri,
    baseName: string
  ): Promise<void> {
    try {
      const executableUri = await DelphiProjectUtils.findExecutableFromDproj(dprojFile);
      if (executableUri) {
        project.executable = executableUri;

        // Look for corresponding INI file next to the executable
        const executableDir = dirname(executableUri.fsPath);
        const executableName = basename(executableUri.fsPath).replace(/\.[^/.]+$/, "");
        const iniPath = join(executableDir, `${executableName}.ini`);

        try {
          await fs.access(iniPath);
          project.ini = Uri.file(iniPath);
        } catch {
          // INI file doesn't exist, that's fine
        }
      }
    } catch (error) {
      console.error(`ProjectDiscovery: Failed to parse executable from DPROJ (${baseName}):`, error);
    }
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async processDprojFiles(): Promise<void> {
    // This method is kept for backward compatibility but not used
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async processStandaloneDprFiles(): Promise<void> {
    // This method is kept for backward compatibility but not used
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async processStandaloneDpkFiles(): Promise<void> {
    // This method is kept for backward compatibility but not used
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async findCorrespondingFile(): Promise<Uri | undefined> {
    // This method is kept for backward compatibility but not used
    return undefined;
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async processExecutableFromDproj(): Promise<void> {
    // This method is kept for backward compatibility but not used
  }
}
