import { TreeItem, TreeDataProvider, TreeItemCollapsibleState, EventEmitter, Event, Uri, workspace, RelativePattern, ConfigurationChangeEvent } from 'vscode';
import { basename, dirname, join } from 'path';
import { promises as fs } from 'fs';
import { DOMParser } from '@xmldom/xmldom';
import { DelphiProjectTreeItem } from './DelphiProjectTreeItem';
import { DelphiProject, ProjectType } from './DelphiProject';
import { DprFile } from './DprFile';
import { DprojFile } from './DprojFile';
import { DpkFile } from './DpkFile';
import { ExecutableFile } from './ExecutableFile';
import { IniFile } from './IniFile';
import { DelphiProjectUtils } from './utils';

/**
 * Interface for the project cache structure.
 * Version 1.0 supports default project discovery with future groupproj functionality.
 */
interface ProjectCacheData {
  lastUpdated: string;
  version: string;
  defaultProjects: ProjectData[];
  groupProjects?: GroupProjectData[];
}

interface ProjectData {
  name: string;
  type: string;
  hasDproj: boolean;
  dprojPath?: string;
  dprojAbsolutePath?: string;
  hasDpr: boolean;
  dprPath?: string;
  dprAbsolutePath?: string;
  hasDpk: boolean;
  dpkPath?: string;
  dpkAbsolutePath?: string;
  hasExecutable: boolean;
  executablePath?: string;
  executableAbsolutePath?: string;
  hasIni: boolean;
  iniPath?: string;
  iniAbsolutePath?: string;
}

interface GroupProjectData {
  name: string;
  groupProjPath: string;
  groupProjAbsolutePath: string;
  projects: ProjectData[];
}

/**
 * Provides a tree view of Delphi projects found in the workspace.
 *
 * Currently supports discovery of individual projects based on configuration.
 * Future versions will support .groupproj files for project grouping.
 *
 * Configuration:
 * - `delphi-utils.delphiProjects.projectPaths`: Array of glob patterns specifying where to search for projects (default: ["**"])
 * - `delphi-utils.delphiProjects.excludePatterns`: Array of glob patterns specifying paths to exclude from search
 *
 * Example settings.json:
 * ```
 * {
 *   "delphi-utils.delphiProjects.projectPaths": ["src/**", "projects/**"],
 *   "delphi-utils.delphiProjects.excludePatterns": ["&#42;&#42;/temp/&#42;&#42;", "&#42;&#42;/backup/&#42;&#42;", "&#42;&#42;/__history/&#42;&#42;"]
 * }
 * ```
 */
export class DelphiProjectsProvider implements TreeDataProvider<DelphiProjectTreeItem> {
  private _onDidChangeTreeData: EventEmitter<DelphiProjectTreeItem | undefined | null | void> = new EventEmitter<DelphiProjectTreeItem | undefined | null | void>();
  readonly onDidChangeTreeData: Event<DelphiProjectTreeItem | undefined | null | void> = this._onDidChangeTreeData.event;
  private configFileName = 'delphiProjects.json';

  constructor() {
    // Watch for file system changes to refresh the tree (case-insensitive patterns)
    const dprWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Rr]');
    const dpkWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Kk]');
    const dprojWatcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Rr][Oo][Jj]');
    const iniWatcher = workspace.createFileSystemWatcher('**/*.[Ii][Nn][Ii]');
    // Future: const groupProjWatcher = workspace.createFileSystemWatcher('**/*.[Gg][Rr][Oo][Uu][Pp][Pp][Rr][Oo][Jj]');

    [dprWatcher, dpkWatcher, dprojWatcher, iniWatcher].forEach(watcher => {
      watcher.onDidCreate(() => {
        this.refresh();
        this.saveProjectsToConfig();
      });
      watcher.onDidDelete(() => {
        this.refresh();
        this.saveProjectsToConfig();
      });
      watcher.onDidChange(() => this.refresh());
    });

    // Watch for configuration changes
    workspace.onDidChangeConfiguration((event: ConfigurationChangeEvent) => {
      if (event.affectsConfiguration('delphi-utils.delphiProjects.excludePatterns') ||
          event.affectsConfiguration('delphi-utils.delphiProjects.projectPaths')) {
        this.refresh();
        this.saveProjectsToConfig();
      }
    });
  }

  private async getConfigFilePath(): Promise<string | null> {
    if (!workspace.workspaceFolders || workspace.workspaceFolders.length === 0) {
      return null;
    }

    const workspaceRoot = workspace.workspaceFolders[0].uri.fsPath;
    const vscodeDir = join(workspaceRoot, '.vscode');

    // Ensure .vscode directory exists
    try {
      await fs.access(vscodeDir);
    } catch {
      await fs.mkdir(vscodeDir, { recursive: true });
    }

    return join(vscodeDir, this.configFileName);
  }

  private async saveProjectsToConfig(): Promise<void> {
    const configPath = await this.getConfigFilePath();
    if (!configPath) {
      return;
    }

    try {
      const projects = await this.getAllProjects();
      const configData = {
        lastUpdated: new Date().toISOString(),
        version: '1.0',
        defaultProjects: projects.map(project => ({
          name: project.label,
          type: project.projectType,
          hasDproj: !!project.dproj,
          dprojPath: project.dproj ? workspace.asRelativePath(project.dproj) : undefined,
          dprojAbsolutePath: project.dproj?.fsPath,
          hasDpr: !!project.dpr,
          dprPath: project.dpr ? workspace.asRelativePath(project.dpr) : undefined,
          dprAbsolutePath: project.dpr?.fsPath,
          hasDpk: !!project.dpk,
          dpkPath: project.dpk ? workspace.asRelativePath(project.dpk) : undefined,
          dpkAbsolutePath: project.dpk?.fsPath,
          hasExecutable: !!project.executable,
          executablePath: project.executable ? workspace.asRelativePath(project.executable) : undefined,
          executableAbsolutePath: project.executable?.fsPath,
          hasIni: !!project.ini,
          iniPath: project.ini ? workspace.asRelativePath(project.ini) : undefined,
          iniAbsolutePath: project.ini?.fsPath
        })),
        // Future: groupProjects will be added here for .groupproj file support
        groupProjects: []
      };

      await fs.writeFile(configPath, JSON.stringify(configData, null, 2), 'utf8');
    } catch (error) {
      console.error('Failed to save Delphi projects to config:', error);
    }
  }

  private async loadDprListFromConfig(): Promise<ProjectCacheData | any> {
    const configPath = await this.getConfigFilePath();
    if (!configPath) {
      return null;
    }

    try {
      const configContent = await fs.readFile(configPath, 'utf8');
      return JSON.parse(configContent);
    } catch {
      // Config file doesn't exist or is invalid, return null
      return null;
    }
  }

  private async getAllProjects(): Promise<DelphiProject[]> {
    console.log('DelphiProjectsProvider: Starting getAllProjects...');

    if (!workspace.workspaceFolders) {
      console.log('DelphiProjectsProvider: No workspace folders found');
      return [];
    }

    console.log(`DelphiProjectsProvider: Found ${workspace.workspaceFolders.length} workspace folders`);

    const projects: DelphiProject[] = [];
    const projectMap = new Map<string, DelphiProject>(); // Key: project base name + directory

    // Get configuration
    const config = workspace.getConfiguration('delphi-utils.delphiProjects');
    const projectPaths: string[] = config.get('projectPaths', ['**']);
    const excludePatterns: string[] = config.get('excludePatterns', []);

    console.log('DelphiProjectsProvider: Project paths:', projectPaths);
    console.log('DelphiProjectsProvider: Exclude patterns:', excludePatterns);

    for (const folder of workspace.workspaceFolders) {
      console.log(`DelphiProjectsProvider: Processing folder: ${folder.uri.fsPath}`);

      // Create exclude pattern for workspace.findFiles
      const excludeGlob = excludePatterns.length > 0 ? `{${excludePatterns.join(',')}}` : undefined;
      console.log('DelphiProjectsProvider: Using exclude glob:', excludeGlob);

      // Search for DPROJ files in each project path
      console.log('DelphiProjectsProvider: Searching for DPROJ files...');
      let allDprojFiles: Uri[] = [];

      for (const projectPath of projectPaths) {
        const dprojPattern = new RelativePattern(folder, `${projectPath}/*.[Dd][Pp][Rr][Oo][Jj]`);
        const dprojFiles = await workspace.findFiles(dprojPattern, excludeGlob);
        allDprojFiles.push(...dprojFiles);
      }

      console.log(`DelphiProjectsProvider: Found ${allDprojFiles.length} DPROJ files after filtering`);

      // Create projects from DPROJ files
      for (const dprojFile of allDprojFiles) {
        const fileName = basename(dprojFile.fsPath);
        const baseName = fileName.replace(/\.[^/.]+$/, "");
        const dirPath = dirname(dprojFile.fsPath);
        const projectKey = `${baseName}-${dirPath}`;

        // Determine project type by checking for DPK vs DPR
        let projectType = ProjectType.Application;

        // Check for DPK file (package) in the same project paths
        let correspondingDpk: Uri | undefined;
        for (const projectPath of projectPaths) {
          const dpkPattern = new RelativePattern(folder, `${projectPath}/${baseName}.[Dd][Pp][Kk]`);
          const dpkFiles = await workspace.findFiles(dpkPattern, excludeGlob);
          correspondingDpk = dpkFiles.find(dpkFile => dirname(dpkFile.fsPath) === dirPath);
          if (correspondingDpk) {
            projectType = ProjectType.Package;
            break;
          }
        }

        const project = new DelphiProject(baseName, projectType);
        project.dproj = dprojFile;

        // Add DPK file if found
        if (correspondingDpk) {
          project.dpk = correspondingDpk;
        }

        // Look for corresponding DPR file in the same project paths
        let correspondingDpr: Uri | undefined;
        for (const projectPath of projectPaths) {
          const dprPattern = new RelativePattern(folder, `${projectPath}/${baseName}.[Dd][Pp][Rr]`);
          const dprFiles = await workspace.findFiles(dprPattern, excludeGlob);
          correspondingDpr = dprFiles.find(dprFile => dirname(dprFile.fsPath) === dirPath);
          if (correspondingDpr) {
            break;
          }
        }

        if (correspondingDpr) {
          project.dpr = correspondingDpr;
        }

        // Try to parse executable path from DPROJ
        try {
          console.log(`DelphiProjectsProvider: Parsing executable from ${baseName}.dproj...`);
          const executableUri = await DelphiProjectUtils.findExecutableFromDproj(dprojFile);
          if (executableUri) {
            project.executable = executableUri;
            console.log(`DelphiProjectsProvider: Found executable: ${executableUri.fsPath}`);

            // Look for corresponding INI file next to the executable
            const executableDir = dirname(executableUri.fsPath);
            const executableName = basename(executableUri.fsPath).replace(/\.[^/.]+$/, "");
            const iniPath = join(executableDir, `${executableName}.ini`);

            try {
              await fs.access(iniPath);
              project.ini = Uri.file(iniPath);
              console.log(`DelphiProjectsProvider: Found INI file: ${iniPath}`);
            } catch {
              // INI file doesn't exist, that's fine
            }
          } else {
            console.log(`DelphiProjectsProvider: No executable found in ${baseName}.dproj`);
          }
        } catch (error) {
          console.error(`DelphiProjectsProvider: Failed to parse executable from DPROJ (${baseName}):`, error);
        }

        project.updateCollapsibleState();
        projectMap.set(projectKey, project);
        console.log(`DelphiProjectsProvider: Added DPROJ project: ${baseName}`);
      }

      // Also look for standalone DPR files without DPROJ (legacy projects)
      console.log('DelphiProjectsProvider: Searching for standalone DPR files...');
      let allDprFiles: Uri[] = [];

      for (const projectPath of projectPaths) {
        const dprPattern = new RelativePattern(folder, `${projectPath}/*.[Dd][Pp][Rr]`);
        const dprFiles = await workspace.findFiles(dprPattern, excludeGlob);
        allDprFiles.push(...dprFiles);
      }

      console.log(`DelphiProjectsProvider: Found ${allDprFiles.length} DPR files after filtering`);

      for (const dprFile of allDprFiles) {
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

      // Also look for standalone DPK files without DPROJ (legacy packages)
      console.log('DelphiProjectsProvider: Searching for standalone DPK files...');
      let allDpkFiles: Uri[] = [];

      for (const projectPath of projectPaths) {
        const dpkPattern = new RelativePattern(folder, `${projectPath}/*.[Dd][Pp][Kk]`);
        const dpkFiles = await workspace.findFiles(dpkPattern, excludeGlob);
        allDpkFiles.push(...dpkFiles);
      }

      console.log(`DelphiProjectsProvider: Found ${allDpkFiles.length} DPK files after filtering`);

      for (const dpkFile of allDpkFiles) {
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

    console.log(`DelphiProjectsProvider: Finished processing, found ${projectMap.size} total projects`);
    return Array.from(projectMap.values());
  }
  private async loadProjectsFromConfig(): Promise<DelphiProject[] | null> {
    const configData = await this.loadDprListFromConfig();
    if (!configData || !configData.defaultProjects) {
      return null;
    }

    const projects: DelphiProject[] = [];

    for (const projectData of configData.defaultProjects) {
      try {
        // Verify at least one main file exists
        let mainFileExists = false;

        if (projectData.hasDproj && projectData.dprojAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dprojAbsolutePath));
            mainFileExists = true;
          } catch {
            // DPROJ file no longer exists
          }
        }

        if (!mainFileExists && projectData.hasDpr && projectData.dprAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dprAbsolutePath));
            mainFileExists = true;
          } catch {
            // DPR file no longer exists
          }
        }

        if (!mainFileExists && projectData.hasDpk && projectData.dpkAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dpkAbsolutePath));
            mainFileExists = true;
          } catch {
            // DPK file no longer exists
          }
        }

        if (!mainFileExists) {
          continue; // Skip this project as no main files exist
        }

        const project = new DelphiProject(projectData.name, projectData.type || ProjectType.Application);

        // Restore file references if they exist
        if (projectData.hasDproj && projectData.dprojAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dprojAbsolutePath));
            project.dproj = Uri.file(projectData.dprojAbsolutePath);
          } catch {
            // File no longer exists
          }
        }

        if (projectData.hasDpr && projectData.dprAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dprAbsolutePath));
            project.dpr = Uri.file(projectData.dprAbsolutePath);
          } catch {
            // File no longer exists
          }
        }

        if (projectData.hasDpk && projectData.dpkAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.dpkAbsolutePath));
            project.dpk = Uri.file(projectData.dpkAbsolutePath);
          } catch {
            // File no longer exists
          }
        }

        if (projectData.hasExecutable && projectData.executableAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.executableAbsolutePath));
            project.executable = Uri.file(projectData.executableAbsolutePath);
          } catch {
            // File no longer exists
          }
        }

        if (projectData.hasIni && projectData.iniAbsolutePath) {
          try {
            await workspace.fs.stat(Uri.file(projectData.iniAbsolutePath));
            project.ini = Uri.file(projectData.iniAbsolutePath);
          } catch {
            // File no longer exists
          }
        }

        project.updateCollapsibleState();
        projects.push(project);
      } catch {
        // Project data is invalid, skip it
        continue;
      }
    }

    return projects;
  }

  /**
   * Gets the current cache structure - useful for debugging and future groupproj development.
   * @returns The current cache data or null if no cache exists
   */
  async getCurrentCacheStructure(): Promise<ProjectCacheData | any> {
    return await this.loadDprListFromConfig();
  }

  refresh(): void {
    this._onDidChangeTreeData.fire(undefined);
  }

  getTreeItem(element: DelphiProjectTreeItem): TreeItem {
    return element;
  }

  async getChildren(element?: DelphiProjectTreeItem): Promise<DelphiProjectTreeItem[]> {
    try {
      if (!element) {
        console.log('DelphiProjectsProvider: Loading root projects...');

        // Root level - try to load from config first, then fall back to file system search
        let projects: DelphiProject[] | null = await this.loadProjectsFromConfig();

        if (!projects || projects.length === 0) {
          console.log('DelphiProjectsProvider: No cached projects found, searching file system...');

          // Config doesn't exist or is empty, do file system search with timeout
          try {
            projects = await Promise.race([
              this.getAllProjects(),
              new Promise<DelphiProject[]>((_, reject) =>
                setTimeout(() => reject(new Error('Project search timed out after 30 seconds')), 30000)
              )
            ]);
          } catch (error) {
            console.error('DelphiProjectsProvider: Project search failed or timed out:', error);
            projects = [];
          }

          console.log(`DelphiProjectsProvider: Found ${projects.length} projects`);

          // Save the current list to config file (async, don't wait)
          this.saveProjectsToConfig().catch((error: any) => {
            console.error('Failed to save Delphi projects:', error);
          });
        } else {
          console.log(`DelphiProjectsProvider: Loaded ${projects.length} projects from cache`);
        }

        // Sort projects alphabetically
        projects.sort((a: DelphiProject, b: DelphiProject) => a.label.localeCompare(b.label));

        return projects;
    } else if (element instanceof DelphiProject) {
      // Delphi project - return constituent files as children
      const children: DelphiProjectTreeItem[] = [];

      if (element.dproj) {
        const dprojFileName = basename(element.dproj.fsPath);
        children.push(new DprojFile(dprojFileName, element.dproj));
      }

      if (element.dpr) {
        const dprFileName = basename(element.dpr.fsPath);
        children.push(new DprFile(dprFileName, element.dpr));
      }

      if (element.dpk) {
        const dpkFileName = basename(element.dpk.fsPath);
        children.push(new DpkFile(dpkFileName, element.dpk));
      }

      if (element.executable) {
        const executableFileName = basename(element.executable.fsPath);
        const executableItem = new ExecutableFile(
          executableFileName,
          element.executable,
          element.ini ? TreeItemCollapsibleState.Collapsed : TreeItemCollapsibleState.None
        );
        executableItem.ini = element.ini;
        children.push(executableItem);
      }

      return children;
    } else if (element instanceof ExecutableFile && element.ini) {
      // Executable file with INI - return INI as child
      const children: DelphiProjectTreeItem[] = [];
      const iniFileName = basename(element.ini.fsPath);
      children.push(new IniFile(iniFileName, element.ini));
      return children;
    }

    return [];
    } catch (error) {
      console.error('DelphiProjectsProvider: Error in getChildren:', error);
      return [];
    }
  }
}
