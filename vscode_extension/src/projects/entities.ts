import { Option } from '../types';
import { fuseStartParameters } from '../utils';

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

  // Returns the TRIMMED value: a whitespace-padded path would reach spawn()
  // verbatim and fail to launch.
  const notBlank = (value?: Option<string>) => {
    const trimmed = value?.trim();
    return trimmed && trimmed.length > 0 ? trimmed : undefined;
  };

  /**
   * The configured hosting executable: the DevKit "Set Host Application"
   * override first, then the dproj's own `Debugger_HostApplication`. Blank
   * values count as absent, and so does a value still containing an
   * unresolved `$(...)` macro — not a launchable path, and it must never
   * shadow the project's own exe. Mirrors `Project::effective_host_application`
   * on the Rust side.
   */
  export function effectiveHostApplication(entity: Project): string | undefined {
    const usable = (value?: Option<string>) => {
      const present = notBlank(value);
      return present && !present.includes('$(') ? present : undefined;
    };
    return usable(entity.host_application) ?? usable(entity.dproj_host_application);
  }

  /**
   * The executable RunProgram launches for a project: a configured Host
   * Application wins over the project's exe, matching the Delphi IDE's Run
   * behaviour — it is what makes a `.dpk` package or DLL project runnable
   * at all.
   */
  export function resolveRunTarget(entity: Project): string | undefined {
    return effectiveHostApplication(entity) ?? notBlank(entity.exe);
  }

  /**
   * The effective command-line parameters RunProgram passes: the dproj's
   * `Debugger_RunParams` fused with the saved Start Parameters (dproj first)
   * when `useDprojRunParams` is enabled, otherwise only the saved value.
   */
  export function resolveEffectiveStartParameters(entity: Project, useDprojRunParams: boolean): string | undefined {
    if (useDprojRunParams) return fuseStartParameters(entity.dproj_run_params, entity.start_parameters);
    return entity.start_parameters ?? undefined;
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
