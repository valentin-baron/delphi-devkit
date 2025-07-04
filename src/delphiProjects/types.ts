/**
 * Type definitions for the Delphi Projects cache and data structures.
 * Version 1.0 supports default project discovery with future groupproj functionality.
 */

/**
 * Interface for the project cache structure.
 */
export interface ProjectCacheData {
  lastUpdated: string;
  version: string;
  defaultProjects: ProjectData[];
  /**
   * Stores the currently picked group project (if any)
   */
  currentGroupProject?: {
    groupProjPath: string;
    groupProjAbsolutePath: string;
    name: string;
    projects: ProjectData[];
  };
}

/**
 * Interface for individual project data in the cache.
 */
export interface ProjectData {
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

/**
 * Interface for group project data (future .groupproj support).
 */
export interface GroupProjectData {
  name: string;
  groupProjPath: string;
  groupProjAbsolutePath: string;
  projects: ProjectData[];
}
