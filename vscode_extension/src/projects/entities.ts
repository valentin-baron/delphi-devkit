import { Option } from '../types';

export namespace Entities {
  export class ProjectsData {
    workspaces: Workspace[];
    projects: Project[];
    group_project?: Option<GroupProject>;
    active_project_id?: Option<number>;
    group_project_compiler_id?: Option<string>;
  }

  export class Workspace {
    id: number;
    name: string;
    compiler_id: string;
    project_links: ProjectLink[];
    sort_rank: string;
    active_configuration?: Option<string>;
    active_platform?: Option<string>;
  }

  export class GroupProject {
    name: string;
    path: string;
    project_links: ProjectLink[];
    active_configuration?: Option<string>;
    active_platform?: Option<string>;
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
    active_configuration?: Option<string>;
    active_platform?: Option<string>;
    start_parameters?: Option<string>;
  }

  export class ProjectLink {
    id: number;
    project_id: number;
    sort_rank: string;
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
