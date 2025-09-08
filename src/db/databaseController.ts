/**
 * Controller class for ease of accessing and manipulating the database.
 */

import { AppDataSource } from './datasource';
import { Entities } from './entities';
import { Runtime } from '../runtime';
import { LexoSorter } from '../utils/lexoSorter';
import { DynamicObject } from '../typings';

export class DatabaseController {
  public async clear(): Promise<void> {
    await AppDataSource.resetDatabase();
  }

  public async getConfiguration(): Promise<Entities.Configuration> {
    let config: Entities.Configuration | null = null;
    await AppDataSource.transaction(async (manager) => {
      config = await manager.findOne(Entities.Configuration, {
        where: { id: 0 }
      });
      if (!config) {
        config = new Entities.Configuration();
        config.id = 0;
        config.groupProjectsCompiler = Runtime.compilerConfigurations[0]?.name || null;
        await manager.save(config);
      }
    });
    if (!config) throw new Error('Something went wrong when reading or creating cache for DDK.');
    config = config as Entities.Configuration;
    config.workspaces = (config.workspaces || []).sort((a, b) => a.sortValue.localeCompare(b.sortValue));
    return config!;
  }

  public async save<T extends DynamicObject>(entity: T): Promise<T> {
    if (entity)
      await AppDataSource.transaction(async (manager) => {
        await manager.save(entity);
      });

    await Runtime.refreshConfigEntity();
    return entity;
  }

  public async saveAll(entities: DynamicObject[]): Promise<DynamicObject[]> {
    if (entities && entities.length > 0)
      await AppDataSource.transaction(async (manager) => {
        await manager.save(entities);
      });
    await Runtime.refreshConfigEntity();
    return entities;
  }

  public async removeWorkspace(workspace: Entities.Workspace): Promise<void> {
    if (workspace)
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(workspace);
      });
    await Runtime.refreshConfigEntity();
  }

  public async addGroupProject(name: string, path: string, projects: Entities.Project[]): Promise<Entities.GroupProject> {
    const groupProject = new Entities.GroupProject();
    groupProject.name = name;
    groupProject.path = path;
    groupProject.projects = projects.map((proj) => {
      const link = new Entities.GroupProjectLink();
      link.project = proj;
      link.groupProject = groupProject;
      return link;
    });
    groupProject.projects = new LexoSorter(groupProject.projects).items;
    await AppDataSource.transaction(async (manager) => {
      await manager.save(groupProject);
    });
    return groupProject;
  }

  public async removeGroupProject(groupProject: Entities.GroupProject): Promise<void> {
    if (groupProject)
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(groupProject);
      });
    await Runtime.refreshConfigEntity();
  }

  public async getGroupProject(path: string): Promise<Entities.GroupProject | null> {
    let groupProject: Entities.GroupProject | null = null;
    await AppDataSource.readConnection(async (manager) => {
      groupProject = await manager.findOne(Entities.GroupProject, {
        where: { path: path }
      });
    });
    return groupProject;
  }

  public async removeProjectLink(link: Entities.ProjectLink): Promise<void> {
    if (link)
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(link);
        const workspaceLinks = await manager.find(Entities.WorkspaceLink, {
          where: { project: link.project }
        });
        const groupProjectLinks = await manager.find(Entities.GroupProjectLink, { where: { project: link.project } });
        if (workspaceLinks.length === 0 && groupProjectLinks.length === 0) await manager.remove(link.project);
      });
    await Runtime.refreshConfigEntity();
  }

  public async removeProject(project: Entities.Project): Promise<void> {
    if (project)
      await AppDataSource.transaction(async (manager) => {
        await manager.remove(project);
      });
    await Runtime.refreshConfigEntity();
  }
}
