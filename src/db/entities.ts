import "reflect-metadata";
import { Column, Entity, JoinColumn, ManyToOne, OneToMany, OneToOne, PrimaryGeneratedColumn } from "typeorm";
import { SortedItem } from "../utils/lexoSorter";

export namespace Entities {
  @Entity()
  export class Configuration {
    @Column({primary: true, type: "int", default: 0})
    id: number;

    @OneToMany(() => Workspace, (workspace) => workspace.configuration, { cascade: true, eager: true })
    workspaces: Workspace[];

    @Column({ type: "varchar", length: 50, nullable: true })
    groupProjectsCompiler?: string | null;

    // Remove cascade here - you want to manually create these references
    @OneToOne(() => Project, { nullable: true, eager: true })
    @JoinColumn()
    selectedProject?: Project | null;

    @OneToOne(() => GroupProject, { nullable: true, eager: true })
    @JoinColumn()
    selectedGroupProject?: GroupProject | null;
  }

  export interface ProjectOwner {
    id: number;
    name: string;
    projects: ProjectLink[];
    selectedProject?: Project;
  }

  @Entity()
  export class Workspace implements ProjectOwner, SortedItem {
    @PrimaryGeneratedColumn()
    id: number;

    @ManyToOne(() => Configuration, (configuration) => configuration.workspaces)
    configuration: Configuration;

    @Column({ type: "varchar", length: 50 })
    name: string;

    @Column({ type: "varchar", length: 50 })
    compiler: string;

    @OneToMany(() => WorkspaceProjectLink, (workspaceProject) => workspaceProject.workspace, { cascade: true, eager: true })
    projects: WorkspaceProjectLink[];

    // Remove cascade here - you want to manually create this reference
    @ManyToOne(() => Project, { nullable: true, eager: true })
    selectedProject?: Project;

    @Column({ type: "varchar", length: 1024 })
    sortValue: string;
  }

  @Entity()
  export class GroupProject implements ProjectOwner {
    @PrimaryGeneratedColumn()
    id: number;

    @Column({ type: "varchar", length: 50 })
    name: string;

    @Column({ type: "varchar", length: 255 })
    path: string;

    @OneToMany(() => GroupProjectProjectLink, (groupProjectProject) => groupProjectProject.groupProject, { cascade: true, eager: true })
    projects: GroupProjectProjectLink[];
  }

  @Entity()
  export class Project {
    @PrimaryGeneratedColumn()
    id: number;

    // Remove onDelete here - it should be on the ManyToOne side
    @OneToMany(() => WorkspaceProjectLink, (workspaceProject) => workspaceProject.project)
    workspaces: WorkspaceProjectLink[];

    @OneToMany(() => GroupProjectProjectLink, (groupProjectProject) => groupProjectProject.project)
    groupProjects: GroupProjectProjectLink[];

    @Column({ type: "varchar", length: 50 })
    name: string;

    @Column({ type: "varchar", length: 255 })
    path: string;

    @Column({ type: "text", nullable: true })
    dproj?: string | null;

    @Column({ type: "text", nullable: true })
    dpr?: string | null;

    @Column({ type: "text", nullable: true })
    dpk?: string | null;

    @Column({ type: "text", nullable: true })
    exe?: string | null;

    @Column({ type: "text", nullable: true })
    ini?: string | null;
  }

  export interface ProjectLink extends SortedItem {
    id: number;
    project: Project;
    sortValue: string;
    owner: ProjectOwner;
  }

  // Join entity for WorkspaceEntity and ProjectEntity with sort value
  @Entity()
  export class WorkspaceProjectLink implements ProjectLink {
    @PrimaryGeneratedColumn()
    id: number;

    // Remove eager and cascade from the "many" side back to workspace
    @ManyToOne(() => Workspace, workspace => workspace.projects)
    workspace: Workspace;

    // Keep cascade for saving projects through links
    @ManyToOne(() => Project, project => project.workspaces, { eager: true, cascade: true })
    project: Project;

    @Column({ type: "varchar", length: 1024 })
    sortValue: string;

    get owner(): Workspace {
      return this.workspace;
    }
  }

  // Join entity for GroupProjectEntity and ProjectEntity with sort value
  @Entity()
  export class GroupProjectProjectLink implements ProjectLink {
    @PrimaryGeneratedColumn()
    id: number;

    // Remove eager and cascade from the "many" side back to groupProject
    @ManyToOne(() => GroupProject, groupProject => groupProject.projects)
    groupProject: GroupProject;

    // Keep cascade for saving projects through links
    @ManyToOne(() => Project, project => project.groupProjects, { eager: true, cascade: true })
    project: Project;

    @Column({ type: "varchar", length: 1024 })
    sortValue: string;

    get owner(): GroupProject {
      return this.groupProject;
    }
  }

  export const ALL = [
    Configuration,
    Workspace,
    GroupProject,
    Project,
    WorkspaceProjectLink,
    GroupProjectProjectLink
  ];
}