import 'reflect-metadata';
import { Column, Entity, JoinColumn, ManyToOne, OneToMany, OneToOne, PrimaryGeneratedColumn } from 'typeorm';
import { SortedItem } from '../utils/lexoSorter';
import { ProjectLinkType } from '../types';
import { Runtime } from '../runtime';

export namespace Entities {
  @Entity()
  export class Configuration {
    @Column({ primary: true, type: 'int', default: 0 })
    id: number;

    @OneToMany(() => Workspace, (workspace) => workspace.configuration, {
      cascade: true,
      eager: true
    })
    workspaces: Workspace[];

    @Column({ type: 'varchar', length: 50, nullable: true })
    groupProjectsCompiler?: string | null;

    @OneToOne(() => Project, { nullable: true, eager: true })
    @JoinColumn()
    selectedProject?: Project | null;

    @OneToOne(() => GroupProject, { nullable: true, eager: true })
    @JoinColumn()
    selectedGroupProject?: GroupProject | null;

    public static clone(config: Partial<Configuration>): Configuration {
      const result = new this();
      result.id = 0;
      result.groupProjectsCompiler = config.groupProjectsCompiler || null;
      if (config.selectedGroupProject) {
        const gp = new GroupProject();
        result.selectedGroupProject = gp;
        gp.name = config.selectedGroupProject.name || '';
        gp.path = config.selectedGroupProject.path || '';
        gp.projects = (config.selectedGroupProject.projects || []).map((link) => {
          if (!link || !link.project) return null;
          const newLink = new GroupProjectLink();
          newLink.groupProject = gp;
          newLink.project = this.cloneProject(link.project);
          newLink.sortValue = link.sortValue || '';
          return newLink;
        }).filter((link): link is GroupProjectLink => link !== null);
      }
      result.workspaces = [];
      config.workspaces?.forEach((ws) => {
        if (!ws) return;
        const newWs = new Workspace();
        newWs.name = ws.name || '';
        newWs.compiler = ws.compiler || (Runtime.compilerConfigurations[0]?.name || '');
        newWs.sortValue = ws.sortValue || '';
        newWs.configuration = result;
        newWs.projects = (ws.projects || []).map((link) => {
          if (!link || !link.project) return null;
          const newLink = new WorkspaceLink();
          newLink.workspace = newWs;
          newLink.project = this.cloneProject(link.project);
          newLink.sortValue = link.sortValue || '';
          return newLink;
        }).filter((link): link is WorkspaceLink => link !== null);
        result.workspaces.push(newWs);
      });
      if (config.selectedProject) {
        const wsProject = result.workspaces.flatMap(ws => ws.projects).find(link => link.project.id === config.selectedProject?.id);
        if (wsProject) result.selectedProject = wsProject.project;
        else {
          const gpProject = result.selectedGroupProject?.projects.find(link => link.project.id === config.selectedProject?.id);
          if (gpProject) result.selectedProject = gpProject.project;
        }
      }
      return result;
    }

    private static cloneProject(proj: Partial<Project>): Project {
      const newProj = new Project();
      newProj.name = proj.name || '';
      newProj.path = proj.path || '';
      newProj.dpr = proj.dpr || null;
      newProj.dpk = proj.dpk || null;
      newProj.dproj = proj.dproj || null;
      newProj.exe = proj.exe || null;
      newProj.ini = proj.ini || null;
      return newProj;
    }
  }

  export interface ProjectOwner {
    id: number;
    name: string;
    projects: ProjectLink[];
  }

  @Entity()
  export class Workspace implements ProjectOwner, SortedItem {
    @PrimaryGeneratedColumn()
    id: number;

    @ManyToOne(() => Configuration, (configuration) => configuration.workspaces)
    configuration: Configuration;

    @Column({ type: 'varchar', length: 50 })
    name: string;

    @Column({ type: 'varchar', length: 50 })
    compiler: string;

    @OneToMany(() => WorkspaceLink, (workspaceLink) => workspaceLink.workspace, { cascade: true, eager: true })
    projects: WorkspaceLink[];

    @Column({ type: 'varchar', length: 1024 })
    sortValue: string;
  }

  @Entity()
  export class GroupProject implements ProjectOwner {
    @PrimaryGeneratedColumn()
    id: number;

    @Column({ type: 'varchar', length: 50 })
    name: string;

    @Column({ type: 'varchar', length: 255 })
    path: string;

    @OneToMany(() => GroupProjectLink, (groupProjectLink) => groupProjectLink.groupProject, { cascade: true, eager: true })
    projects: GroupProjectLink[];
  }

  @Entity()
  export class Project {
    @PrimaryGeneratedColumn()
    id: number;

    @OneToMany(() => WorkspaceLink, (workspaceLink) => workspaceLink.project)
    workspaces: WorkspaceLink[];

    @OneToMany(() => GroupProjectLink, (groupProjectProject) => groupProjectProject.project)
    groupProjects: GroupProjectLink[];

    @Column({ type: 'varchar', length: 50 })
    name: string;

    @Column({ type: 'varchar', length: 255 })
    path: string;

    @Column({ type: 'text', nullable: true })
    dproj?: string | null;

    @Column({ type: 'text', nullable: true })
    dpr?: string | null;

    @Column({ type: 'text', nullable: true })
    dpk?: string | null;

    @Column({ type: 'text', nullable: true })
    exe?: string | null;

    @Column({ type: 'text', nullable: true })
    ini?: string | null;
  }

  export interface ProjectLink extends SortedItem {
    id: number;
    project: Project;
    sortValue: string;
    linkType: ProjectLinkType;

    workspaceSafe: Workspace | undefined | null;
    groupProjectSafe: GroupProject | undefined | null;
  }

  // Join entity for WorkspaceEntity and ProjectEntity with sort value
  @Entity()
  export class WorkspaceLink implements ProjectLink {
    @PrimaryGeneratedColumn()
    id: number;

    @ManyToOne(() => Workspace, (workspace) => workspace.projects)
    workspace: Workspace;

    @ManyToOne(() => Project, (project) => project.workspaces, {
      eager: true,
      cascade: true
    })
    project: Project;

    @Column({ type: 'varchar', length: 1024 })
    sortValue: string;

    get linkType(): ProjectLinkType {
      return ProjectLinkType.Workspace;
    }

    get workspaceSafe(): Workspace | undefined | null {
      if (this.workspace) return this.workspace;

      for (const ws of Runtime.configEntity.workspaces ?? []) if (ws.projects.some((link) => link.id === this.id)) return ws;
    }

    get groupProjectSafe(): null {
      return null;
    }
  }

  @Entity()
  export class GroupProjectLink implements ProjectLink {
    @PrimaryGeneratedColumn()
    id: number;

    @ManyToOne(() => GroupProject, (groupProject) => groupProject.projects)
    groupProject: GroupProject;

    @ManyToOne(() => Project, (project) => project.groupProjects, {
      eager: true,
      cascade: true
    })
    project: Project;

    @Column({ type: 'varchar', length: 1024 })
    sortValue: string;

    get linkType(): ProjectLinkType {
      return ProjectLinkType.GroupProject;
    }

    get workspaceSafe(): null {
      return null;
    }

    get groupProjectSafe(): GroupProject | undefined {
      if (this.groupProject) return this.groupProject;

      if (Runtime.configEntity.selectedGroupProject?.projects.some((link) => link.id === this.id)) return Runtime.configEntity.selectedGroupProject;
    }
  }

  export const ALL = [Configuration, Workspace, GroupProject, Project, WorkspaceLink, GroupProjectLink];
}
