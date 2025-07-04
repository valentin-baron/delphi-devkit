import { TreeItem, TreeDataProvider, TreeItemCollapsibleState, EventEmitter, Event, Uri, workspace, ConfigurationChangeEvent, window, ProgressLocation } from 'vscode';
import { basename } from 'path';
import { DelphiProjectTreeItem } from './treeItems/DelphiProjectTreeItem';
import { DelphiProject } from './treeItems/DelphiProject';
import { DprFile } from './treeItems/DprFile';
import { DprojFile } from './treeItems/DprojFile';
import { DpkFile } from './treeItems/DpkFile';
import { ExecutableFile } from './treeItems/ExecutableFile';
import { IniFile } from './treeItems/IniFile';
import { ProjectCacheData } from './types';
import { ProjectCacheManager } from './data/cacheManager';
import { ProjectDiscovery } from './data/projectDiscovery';
import { ProjectLoader } from './data/projectLoader';
import { minimatch } from 'minimatch';

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
  private cacheManager = new ProjectCacheManager();
  private forceRefreshCache = false;

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
        // Removed: this.saveProjectsToConfig();
      });
      watcher.onDidDelete(() => {
        this.refresh();
        // Removed: this.saveProjectsToConfig();
      });
      watcher.onDidChange(() => this.refresh());
    });

    // Watch for configuration changes
    workspace.onDidChangeConfiguration((event: ConfigurationChangeEvent) => {
      if (event.affectsConfiguration('delphi-utils.delphiProjects.excludePatterns') ||
          event.affectsConfiguration('delphi-utils.delphiProjects.projectPaths')) {
        this.refresh();
        // Removed: this.saveProjectsToConfig();
      }
    });
  }

  private async saveProjectsToConfig(): Promise<void> {
    try {
      const projects = await ProjectDiscovery.getAllProjects();
      const configData: ProjectCacheData = {
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
        }))
        // groupProjects removed
      };

      await this.cacheManager.saveCacheData(configData);
    } catch (error) {
      console.error('Failed to save Delphi projects to config:', error);
    }
  }

  /**
   * Gets the current cache structure - useful for debugging and future groupproj development.
   * @returns The current cache data or null if no cache exists
   */
  async getCurrentCacheStructure(): Promise<ProjectCacheData | null> {
    return await this.cacheManager.loadCacheData();
  }

  refresh(forceCache?: boolean): void {
    if (forceCache) {
      this.forceRefreshCache = true;
    }
    this._onDidChangeTreeData.fire(undefined);
  }

  getTreeItem(element: DelphiProjectTreeItem): TreeItem {
    return element;
  }

  async getChildren(element?: DelphiProjectTreeItem): Promise<DelphiProjectTreeItem[]> {
    try {
      if (!element) {
        console.log('DelphiProjectsProvider: Loading root projects...');
        let configData: ProjectCacheData | null = null;
        let projects: DelphiProject[] | null = null;
        configData = await this.cacheManager.loadCacheData();
        // If a group project is loaded, show only its projects
        if (configData && configData.currentGroupProject) {
          // Convert ProjectData[] to DelphiProject[]
          projects = await ProjectLoader.loadProjectsFromConfig({ defaultProjects: configData.currentGroupProject.projects });
        } else {
          if (this.forceRefreshCache) {
            // Always rebuild cache if forced
            projects = await ProjectDiscovery.getAllProjects();
            await this.saveProjectsToConfig();
            this.forceRefreshCache = false;
          } else {
            projects = await ProjectLoader.loadProjectsFromConfig(configData);
            if (!projects || projects.length === 0) {
              console.log('DelphiProjectsProvider: No cached projects found, searching file system...');
              try {
                projects = await window.withProgress({
                  location: ProgressLocation.Notification,
                  title: "Searching for Delphi projects...",
                  cancellable: false
                }, async (progress) => {
                  progress.report({ message: "Scanning workspace folders..." });
                  const result = await Promise.race([
                    ProjectDiscovery.getAllProjects(),
                    new Promise<DelphiProject[]>((_, reject) =>
                      setTimeout(() => reject(new Error('Project search timed out after 30 seconds')), 30000)
                    )
                  ]);
                  progress.report({ message: `Found ${result.length} projects` });
                  return result;
                });
              } catch (error) {
                console.error('DelphiProjectsProvider: Project search failed or timed out:', error);
                window.showWarningMessage('Delphi project search failed or timed out. Please check your workspace and configuration.');
                projects = [];
              }
              console.log(`DelphiProjectsProvider: Found ${projects.length} projects`);
              await this.saveProjectsToConfig();
            } else {
              console.log(`DelphiProjectsProvider: Loaded ${projects.length} projects from cache`);
            }
          }
        }
        if (!projects) { return []; }
        // Only sort if setting is enabled and not a group project
        const isGroupProject = !!(configData && configData.currentGroupProject);
        if (!isGroupProject) {
          const sortProjects = workspace.getConfiguration('delphi-utils').get<boolean>('delphiProjects.sortProjects', false);
          // Level 1: always sort by projectPaths glob order
          const config = workspace.getConfiguration('delphi-utils.delphiProjects');
          const projectPaths: string[] = config.get('projectPaths', ['**']);
          let orderedProjects: DelphiProject[] = [];
          const used = new Set<DelphiProject>();
          for (const glob of projectPaths) {
            // Find projects whose dpr/dproj/dpk path matches this glob using minimatch
            let group = projects.filter(p => {
              const absPath = p.dpr?.fsPath || p.dproj?.fsPath || p.dpk?.fsPath || '';
              const relPath = absPath ? workspace.asRelativePath(absPath).replace(/\\/g, '/') : '';
              // Use minimatch for proper glob matching
              return minimatch(relPath, glob.replace(/\\/g, '/'));
            }).filter(p => !used.has(p));
            // Level 2: sort within group if enabled
            if (sortProjects) {
              group = group.slice().sort((a, b) => a.label.localeCompare(b.label));
            }
            group.forEach(p => used.add(p));
            orderedProjects = orderedProjects.concat(group);
          }
          return orderedProjects;
        }
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
