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
    /** `Debugger_RunParams` from the dproj (Project > Options > Run in the Delphi IDE). Read-only, refreshed on discovery. */
    dproj_run_params?: Option<string>;
    /** `Debugger_HostApplication` from the dproj (Project > Options > Debugger in the Delphi IDE). Read-only, refreshed on discovery. */
    dproj_host_application?: Option<string>;
    /** DevKit-side Host Application override: the executable run to host this project (e.g. loading a .dpk/BPL). Wins over dproj_host_application. */
    host_application?: Option<string>;
  }

  export class ProjectLink {
    id: number;
    project_id: number;
    sort_rank: string;
  }

  /**
   * The executable RunProgram launches for a project: a configured Host
   * Application (the DevKit override first, then the dproj's own
   * `Debugger_HostApplication`) wins over the project's exe, matching the
   * Delphi IDE's Run behaviour — it is what makes a `.dpk` package or DLL
   * project runnable at all. Blank values count as absent.
   */
  export function resolveRunTarget(entity: Project): string | undefined {
    const notBlank = (value?: Option<string>) => (value && value.trim().length > 0 ? value : undefined);
    return notBlank(entity.host_application) ?? notBlank(entity.dproj_host_application) ?? notBlank(entity.exe);
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
