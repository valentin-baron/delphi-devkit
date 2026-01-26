import { Runtime } from '../runtime';
import { Option } from '../types';

export namespace Entities {
  export class ProjectsData {
    workspaces: Workspace[];
    projects: Project[];
    group_project?: Option<GroupProject>;
    active_project_id?: Option<number>;

    public get active_project(): Option<Project> {
      return this.projects.find((p) => p.id === this.active_project_id);
    }
  }

  export class Workspace {
    id: number;
    name: string;
    compiler_id: string;
    project_links: ProjectLink[];
    sort_rank: string;

    public get compiler(): Option<CompilerConfiguration> {
      try {
        return Runtime.compilerConfigurations?.[this.compiler_id];
      } catch {
        return undefined;
      }
    }
  }

  export class GroupProject {
    name: string;
    path: string;
    compiler_id: string;
    project_links: ProjectLink[];

    public get compiler(): Option<CompilerConfiguration> {
      try {
        return Runtime.compilerConfigurations?.[this.compiler_id];
      } catch {
        return undefined;
      }
    }
  }

  export class Project {
    id: number;
    name: string;
    directory: string;
    dproj?: Option<string>;
    dpr?: Option<string>;
    dpk?: Option<string>;
    exe?: Option<string>;
    ini?: Option<string>;

    public get links(): ProjectLink[] {
      const workspaceLinks = Runtime.projectsData?.workspaces
        .flatMap((ws) => ws.project_links)
        .filter((link) => link.project_id === this.id) || [];
      const groupProjectLinks = Runtime.projectsData?.group_project?.project_links.filter(
        (link) => link.project_id === this.id
      ) || [];
      return [...workspaceLinks, ...groupProjectLinks];
    }
  }

  export class ProjectLink {
    id: number;
    project_id: number;
    sort_rank: string;

    get project(): Option<Project> {
      return Runtime.projectsData?.projects.find((p) => p.id === this.project_id);
    }

    get workspace(): Option<Workspace> {
      return Runtime.projectsData?.workspaces.find((ws) => ws.project_links.some((link) => link.id === this.id));
    }

    get groupProject(): Option<GroupProject> {
      if (Runtime.projectsData?.group_project?.project_links.some((link) => link.id === this.id))
        return Runtime.projectsData?.group_project;

      return undefined;
    }

    public async compile(recreate: boolean = false): Promise<void> {
      return await Runtime.client.compileProject(recreate, this.project_id, this.id);
    }
  }

  export class CompilerConfiguration {
    condition: string;
    product_name: string;
    product_version: number;
    package_version: number;
    compiler_version: number;
    installation_path: string;
    build_arguments: string[];
  }

  export type CompilerConfigurations = {
    [compilerId: string]: CompilerConfiguration;
  }
}
