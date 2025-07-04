import { Uri, workspace } from 'vscode';
import { DelphiProject, ProjectType } from '../treeItems/DelphiProject';
import { ProjectData } from '../types';

/**
 * Service for loading projects from cached configuration data.
 */
export class ProjectLoader {

  /**
   * Load projects from cached configuration data.
   */
  static async loadProjectsFromConfig(configData: any): Promise<DelphiProject[] | null> {
    if (!configData || !configData.defaultProjects) {
      return null;
    }

    const projects: DelphiProject[] = [];

    // Batch file existence checks to reduce I/O operations
    const fileChecks = new Map<string, Promise<boolean>>();

    // Collect all unique file paths that need verification
    const uniqueFilePaths = new Set<string>();
    for (const projectData of configData.defaultProjects) {
      if (projectData.dprojAbsolutePath) {
        uniqueFilePaths.add(projectData.dprojAbsolutePath);
      }
      if (projectData.dprAbsolutePath) {
        uniqueFilePaths.add(projectData.dprAbsolutePath);
      }
      if (projectData.dpkAbsolutePath) {
        uniqueFilePaths.add(projectData.dpkAbsolutePath);
      }
      if (projectData.executableAbsolutePath) {
        uniqueFilePaths.add(projectData.executableAbsolutePath);
      }
      if (projectData.iniAbsolutePath) {
        uniqueFilePaths.add(projectData.iniAbsolutePath);
      }
    }

    // Start all file existence checks in parallel
    for (const filePath of uniqueFilePaths) {
      fileChecks.set(filePath, this.checkFileExists(filePath));
    }

    // Process projects using the batched file checks
    for (const projectData of configData.defaultProjects) {
      try {
        // Verify at least one main file exists using cached checks
        const mainFileExists = await this.verifyMainFileExistsBatch(projectData, fileChecks);
        if (!mainFileExists) {
          continue; // Skip this project as no main files exist
        }

        const project = new DelphiProject(projectData.name, projectData.type || ProjectType.Application);

        // Restore file references if they exist using cached checks
        await this.restoreFileReferencesBatch(project, projectData, fileChecks);
        project.setIcon();
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
   * Check if a file exists (used for batching).
   */
  private static async checkFileExists(filePath: string): Promise<boolean> {
    try {
      await workspace.fs.stat(Uri.file(filePath));
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Verify that at least one main project file exists using batched checks.
   */
  private static async verifyMainFileExistsBatch(
    projectData: ProjectData,
    fileChecks: Map<string, Promise<boolean>>
  ): Promise<boolean> {
    // Check DPROJ file
    if (projectData.hasDproj && projectData.dprojAbsolutePath) {
      const exists = await fileChecks.get(projectData.dprojAbsolutePath);
      if (exists) {
        return true;
      }
    }

    // Check DPR file
    if (projectData.hasDpr && projectData.dprAbsolutePath) {
      const exists = await fileChecks.get(projectData.dprAbsolutePath);
      if (exists) {
        return true;
      }
    }

    // Check DPK file
    if (projectData.hasDpk && projectData.dpkAbsolutePath) {
      const exists = await fileChecks.get(projectData.dpkAbsolutePath);
      if (exists) {
        return true;
      }
    }

    return false;
  }

  /**
   * Restore file references from cached data to project instance using batched checks.
   */
  private static async restoreFileReferencesBatch(
    project: DelphiProject,
    projectData: ProjectData,
    fileChecks: Map<string, Promise<boolean>>
  ): Promise<void> {
    // Restore DPROJ file reference
    if (projectData.hasDproj && projectData.dprojAbsolutePath) {
      const exists = await fileChecks.get(projectData.dprojAbsolutePath);
      if (exists) {
        project.dproj = Uri.file(projectData.dprojAbsolutePath);
      }
    }

    // Restore DPR file reference
    if (projectData.hasDpr && projectData.dprAbsolutePath) {
      const exists = await fileChecks.get(projectData.dprAbsolutePath);
      if (exists) {
        project.dpr = Uri.file(projectData.dprAbsolutePath);
      }
    }

    // Restore DPK file reference
    if (projectData.hasDpk && projectData.dpkAbsolutePath) {
      const exists = await fileChecks.get(projectData.dpkAbsolutePath);
      if (exists) {
        project.dpk = Uri.file(projectData.dpkAbsolutePath);
      }
    }

    // Restore executable file reference
    if (projectData.hasExecutable && projectData.executableAbsolutePath) {
      const exists = await fileChecks.get(projectData.executableAbsolutePath);
      if (exists) {
        project.executable = Uri.file(projectData.executableAbsolutePath);
      }
    }

    // Restore INI file reference
    if (projectData.hasIni && projectData.iniAbsolutePath) {
      const exists = await fileChecks.get(projectData.iniAbsolutePath);
      if (exists) {
        project.ini = Uri.file(projectData.iniAbsolutePath);
      }
    }
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async verifyMainFileExists(): Promise<boolean> {
    return false;
  }

  /**
   * @deprecated Legacy method - now handled by batch processing
   */
  private static async restoreFileReferences(): Promise<void> {
    // This method is kept for backward compatibility but not used
  }
}
