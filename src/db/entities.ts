import "reflect-metadata";
import { Column, Entity, JoinColumn, ManyToOne, OneToMany, OneToOne, PrimaryColumn, PrimaryGeneratedColumn } from "typeorm";
import { WorkspaceViewMode } from "../types";
import { ProjectType } from "../projects/treeItems/delphiProject";
import { SortedItem } from "../utils/lexoSorter";


@Entity()
export class WorkspaceEntity {
  @PrimaryColumn({ type: "text" })
  hash: string;

  @Column({ type: "varchar", length: 50 })
  compiler: string;

  @Column({ type: "datetime" })
  lastUpdated: Date;

  @Column({ type: "text", nullable: true })
  paths?: string;

  @OneToMany(() => ProjectEntity, (project) => project.workspace, { cascade: true, eager: true })
  discoveredProjects: ProjectEntity[];

  @OneToOne(() => GroupProjectEntity, { nullable: true, eager: true, cascade: true })
  @JoinColumn()
  currentGroupProject?: GroupProjectEntity | null;

  @OneToOne(() => ProjectEntity, { nullable: true, eager: true, cascade: true })
  @JoinColumn()
  currentProject?: ProjectEntity;

  public get viewMode(): WorkspaceViewMode {
    return !!this.currentGroupProject ?
      WorkspaceViewMode.GroupProject :
      (
        !!this.discoveredProjects ?
        WorkspaceViewMode.Discovery :
        WorkspaceViewMode.Empty
      );
  }
}

@Entity()
export class GroupProjectEntity {
  @PrimaryGeneratedColumn()
  id: number;

  @Column({ type: "varchar", length: 50 })
  name: string;

  @Column({ type: "varchar", length: 255 })
  path: string;

  @OneToMany(() => ProjectEntity, (project) => project.groupProject, { cascade: true, eager: true })
  projects: ProjectEntity[];

  @OneToOne(() => ProjectEntity, { nullable: true, eager: true, cascade: true })
  @JoinColumn()
  currentProject?: ProjectEntity;
}

@Entity()
export class ProjectEntity implements SortedItem {
  @PrimaryGeneratedColumn()
  id: number;

  @ManyToOne(() => WorkspaceEntity, workspace => workspace.discoveredProjects, { nullable: true })
  workspace?: WorkspaceEntity;

  @ManyToOne(() => GroupProjectEntity, groupProject => groupProject.projects, { nullable: true })
  groupProject?: GroupProjectEntity;

  @Column({ type: "varchar", length: 50 })
  name: string;

  @Column({ type: "varchar", length: 255 })
  path: string;

  @Column({
    type: "varchar",
    length: 50,
    default: ProjectType.Application
  })
  type: ProjectType;

  @Column({ type: "varchar", length: 255, nullable: true })
  dprojPath?: string;

  @Column({ type: "varchar", length: 255, nullable: true })
  dprPath?: string;

  @Column({ type: "varchar", length: 255, nullable: true })
  dpkPath?: string;

  @Column({ type: "varchar", length: 255, nullable: true })
  exePath?: string;

  @Column({ type: "varchar", length: 255, nullable: true })
  iniPath?: string;

  @Column({ type: "varchar", length: 1024 })
  sortValue: string;
}
