import { AppDataSource } from "./datasource";
import { Uri } from "vscode";
import {
  GroupProjectEntity,
  ProjectEntity,
  WorkspaceEntity,
} from "./entities";
import { fileExists } from "../utils";
import { Runtime } from "../runtime";
import { WorkspaceViewMode } from "../types";

export class DatabaseController {
  public async getWorkspace(): Promise<WorkspaceEntity | null> {
    if (!Runtime.assertWorkspaceAvailable()) {
      return null;
    }
    return await AppDataSource.load<WorkspaceEntity>(
      WorkspaceEntity,
      {
        where: {
          hash: Runtime.workspaceHashHumanReadable,
        },
      },
      async () => this.initializeWorkspace()
    );
  }

  public async modify(
    callback: (workspace: WorkspaceEntity) => Promise<WorkspaceEntity | unknown>
  ): Promise<WorkspaceEntity | null> {
    if (!Runtime.assertWorkspaceAvailable()) {
      return null;
    }
    return await AppDataSource.save<WorkspaceEntity>(
      WorkspaceEntity,
      {
        where: {
          hash: Runtime.workspaceHashHumanReadable,
        },
      },
      callback,
      async () => this.initializeWorkspace()
    );
  }

  public async reset(): Promise<void> {
    if (!Runtime.assertWorkspaceAvailable()) { return; }
    await AppDataSource.reset();
  }

  public async initializeWorkspace(): Promise<WorkspaceEntity> {
    const workspace = new WorkspaceEntity();
    workspace.compiler = (await Runtime.projects.compiler.getConfiguration(false)).name;
    workspace.hash = Runtime.workspaceHashHumanReadable;
    workspace.lastUpdated = new Date();
    return workspace;
  }

  public async removeNonExistentFiles(
    inputProjects?: ProjectEntity[]
  ): Promise<ProjectEntity[]> {
    if (!inputProjects) {
      return [];
    }
    const projects: ProjectEntity[] = [];

    // Batch file existence checks to reduce I/O operations
    let changed = false;

    let getChanges = (proj: ProjectEntity): [boolean, ProjectEntity | undefined] => {
      if (!proj) { return [false, proj]; }
      let changeCount = 0;
      if (proj.dprojPath && !fileExists(proj.dprojPath)) {
        proj.dprojPath = undefined;
        changeCount++;
      }
      if (proj.dprPath && !fileExists(proj.dprPath)) {
        proj.dprPath = undefined;
        changeCount++;
      }
      if (proj.dpkPath && !fileExists(proj.dpkPath)) {
        proj.dpkPath = undefined;
        changeCount++;
      }
      if (proj.exePath && !fileExists(proj.exePath)) {
        proj.exePath = undefined;
        changeCount++;
      }
      if (proj.iniPath && !fileExists(proj.iniPath)) {
        proj.iniPath = undefined;
        changeCount++;
      }
      // If all paths are undefined, we consider the project as not existing
      return [ changeCount > 0, changeCount === 5 ? undefined : proj];
    };

    await Promise.all(
      Array.from(inputProjects).map(async (project) => {
        const [chg, proj] = getChanges(project);
        changed ||= chg;
        if (proj) {
          projects.push(proj);
        }
      })
    );

    if (changed) {
      await Runtime.db.modify(async (ws) => {
        ws.lastUpdated = new Date();
        switch (ws.viewMode) {
          case WorkspaceViewMode.GroupProject:
            if (ws.currentGroupProject) {
              ws.currentGroupProject.projects = projects;
            }
            break;
          case WorkspaceViewMode.Discovery:
            ws.discoveredProjects = projects;
            break;
        }
      });
    }

    return projects;
  }

  public async getGroupProject(uri: Uri): Promise<GroupProjectEntity | null> {
    return (
      (await AppDataSource.load<GroupProjectEntity>(GroupProjectEntity, {
        where: { path: uri.fsPath },
      })) || null
    );
  }
}
