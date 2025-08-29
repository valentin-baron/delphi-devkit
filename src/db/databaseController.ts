/**
 * Controller class for ease of accessing and manipulating the database.
 */

import { AppDataSource } from "./datasource";
import { Entities } from "./entities";

export class DatabaseController {
  public async getConfiguration(): Promise<Entities.Configuration> {
    let config: Entities.Configuration | null = null;
    await AppDataSource.transaction(async (manager) => {
      config = await manager.findOne(Entities.Configuration, {});
      if (!config) {
        config = await manager.save(new Entities.Configuration());
      }
    });
    return config!;
  }

  public async saveConfiguration(config: Entities.Configuration): Promise<void> {
    if (config) {
      await AppDataSource.transaction(async (manager) => {
        await manager.save(config);
      });
    }
  }

  public async addWorkspace(name: string, compiler: string): Promise<Entities.Workspace> {
    const workspace = new Entities.Workspace();
    workspace.name = name;
    workspace.compiler = compiler;
    await AppDataSource.transaction(async (manager) => {
      await manager.save(workspace);
    });
    return workspace;
  }

  public async removeWorkspace(workspace: Entities.Workspace): Promise<void> {
    if (workspace) {
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(workspace);
      });
    }
  }

  public async saveWorkspace(workspace: Entities.Workspace): Promise<void> {
    if (workspace) {
      await AppDataSource.transaction(async (manager) => {
        await manager.save(workspace);
      });
    }
  }

  public async getWorkspaces(): Promise<Entities.Workspace[]> {
    return (await this.getConfiguration()).workspaces;
  }

  public async addGroupProject(name: string, path: string, projects: Entities.Project[]): Promise<Entities.GroupProject> {
    const groupProject = new Entities.GroupProject();
    groupProject.name = name;
    groupProject.path = path;
    groupProject.projects = projects.map((proj) => {
      const link = new Entities.GroupProjectProjectLink();
      link.project = proj;
      link.groupProject = groupProject;
      return link;
    });
    await AppDataSource.transaction(async (manager) => {
      await manager.save(groupProject);
    });
    return groupProject;
  }

  public async removeGroupProject(groupProject: Entities.GroupProject): Promise<void> {
    if (groupProject) {
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(groupProject);
      });
    }
  }

  public async saveGroupProject(groupProject: Entities.GroupProject): Promise<void> {
    if (groupProject) {
      await AppDataSource.transaction(async (manager) => {
        await manager.save(groupProject);
      });
    }
  }

  public async getGroupProject(path: string): Promise<Entities.GroupProject | null> {
    let groupProject: Entities.GroupProject | null = null;
    await AppDataSource.readConnection(async (manager) => {
      groupProject = await manager.findOne(Entities.GroupProject, { where: { path: path}});
    });
    return groupProject;
  }

  public async saveProjectLink(link: Entities.ProjectLink): Promise<void> {
    if (link) {
      await AppDataSource.transaction(async (manager) => {
        await manager.save(link);
      });
    }
  }

  public async removeProjectLink(link: Entities.ProjectLink): Promise<void> {
    if (link) {
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(link);
        const workspaceLinks = await manager.find(Entities.WorkspaceProjectLink, { where: { project: link.project }});
        const groupProjectLinks = await manager.find(Entities.GroupProjectProjectLink, { where: { project: link.project }});
        if (workspaceLinks.length === 0 && groupProjectLinks.length === 0) {
          await manager.remove(link.project);
        }
      });
    }
  }

  public async saveProject(project: Entities.Project): Promise<void> {
    if (project) {
      await AppDataSource.transaction(async (manager) => {
        await manager.save(project);
      });
    }
  }

  public async removeProject(project: Entities.Project): Promise<void> {
    if (project) {
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(project);
      });
    }
  }
}
