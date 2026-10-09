import { CancellationToken, DebugConfiguration, DebugConfigurationProvider, window, workspace, WorkspaceFolder } from 'vscode';
import { Runtime } from '../runtime';
import { Entities } from '../projects/entities';
import { DEBUG } from '../constants';
import { CompileOutcome, configurationName, linkToCompile, projectReference, projectReferredTo } from './contract';

function allProjects(): Entities.Project[] {
  return Runtime.projectsData?.projects ?? [];
}

/** The configuration DDK starts for a project, launch or attach. */
export function configurationFor(project: Entities.Project, request: 'launch' | 'attach'): DebugConfiguration {
  return {
    type: DEBUG.TYPE,
    request,
    name: configurationName(request, project, allProjects()),
    ddkProject: projectReference(project, allProjects())
  };
}

/**
 * Contributes DDK's projects to the debug dropdown (dynamic configurations)
 * for the `delphi` debug type. Every entry is the two-line form
 * `{ type, request, ddkProject }`: the debugger extension that owns the type
 * resolves it by asking DDK for the project's debug target, so nothing
 * debugger-specific is written here and a hand-written launch.json entry
 * looks exactly the same. Every project is listed, built or not — a launch
 * builds it first, exactly as *Debug* in the project's context menu does.
 */
export class DdkDebugConfigurationList implements DebugConfigurationProvider {
  async provideDebugConfigurations(_folder: WorkspaceFolder | undefined): Promise<DebugConfiguration[]> {
    return allProjects().flatMap((project) => [configurationFor(project, 'launch'), configurationFor(project, 'attach')]);
  }
}

/**
 * Builds a project before its debug session starts. VS Code asks every
 * provider of a debug type to resolve a configuration, however the session
 * was started — the project's context menu, the debug dropdown, a
 * launch.json entry, or F5 repeating the last session — so this is the one
 * place where "compile before debug" holds for all of them.
 *
 * Only launches of a configuration that names a DDK project are built;
 * anything else passes through untouched, and so does everything when
 * `ddk.debug.compileBeforeDebug` is off. A build that fails or is cancelled
 * aborts the session: debugging the previous binary with the new sources is
 * exactly what the build is there to prevent.
 */
export class DdkBuildBeforeDebug implements DebugConfigurationProvider {
  /** The link the user acted on for the session about to start, if any. */
  private pickedLink?: { project: number; link: number };

  /** Records that the next session of `project` was asked for on `link`. */
  public pick(project: Entities.Project, link: Entities.ProjectLink | undefined): void {
    this.pickedLink = link ? { project: project.id, link: link.id } : undefined;
  }

  async resolveDebugConfiguration(
    folder: WorkspaceFolder | undefined,
    configuration: DebugConfiguration,
    _token?: CancellationToken
  ): Promise<DebugConfiguration | undefined> {
    if (isEmptyConfiguration(configuration)) return this.launchOfTheActiveProject(folder);
    const picked = this.pickedLink;
    this.pickedLink = undefined;
    if (configuration.request !== 'launch' || !compileBeforeDebug()) return configuration;
    const project = projectReferredTo(configuration.ddkProject, allProjects());
    if (!project) {
      // Silence here would be the failure the build exists to prevent: the
      // session starts on whatever binary is lying there. A name that
      // matches nothing, matches several projects, or is asked for before
      // the server has sent its projects all land here.
      window.showWarningMessage(
        `No single DDK project matches "${configuration.ddkProject}", so nothing was compiled: the debug session starts on the binary as it is.`
      );
      return configuration;
    }

    const pickedLinkId = picked?.project === project.id ? picked.link : undefined;
    const link = linkToCompile(Runtime.getLinksOfProject(project), pickedLinkId);
    if (!link) {
      window.showWarningMessage(
        `"${project.name}" belongs to no workspace or group project, so it cannot be compiled: the debug session starts on the binary as it is.`
      );
      return configuration;
    }
    const outcome = await Runtime.client.compileProjectForOutcome(false, link.project_id, link.id, true);
    if (outcome?.success) return configuration;
    const problem = whyNotStarted(project.name, outcome);
    if (problem) window.showErrorMessage(problem);
    // `undefined` aborts the session without opening launch.json.
    return undefined;
  }

  /**
   * F5 with no launch.json (or none selected) hands every provider an empty
   * configuration once the `delphi` debugger is picked. Left empty, the
   * session dies without a word; DDK knows what the user means — its active
   * project — and the configuration goes through the build like any other.
   */
  private async launchOfTheActiveProject(folder: WorkspaceFolder | undefined): Promise<DebugConfiguration | undefined> {
    const project = Runtime.activeProject;
    if (!project) {
      window.showWarningMessage('Select a project in the DDK tree (or add a launch.json) to debug with F5.');
      return undefined;
    }
    return this.resolveDebugConfiguration(folder, configurationFor(project, 'launch'));
  }
}

/** The configuration VS Code passes when there is no launch.json entry to start. */
function isEmptyConfiguration(configuration: DebugConfiguration): boolean {
  return !configuration.type && !configuration.request && !configuration.name;
}

/**
 * What to tell the user when the build did not succeed. Nothing for a build
 * they cancelled themselves: the compiler output has said so already.
 */
function whyNotStarted(project: string, outcome: CompileOutcome | undefined): string | undefined {
  if (!outcome)
    return `The DDK server did not report the outcome of compiling "${project}" (is it older than the extension?); the debug session was not started.`;
  if (outcome.cancelled) return undefined;
  return `Compilation of "${project}" failed; the debug session was not started.`;
}

function compileBeforeDebug(): boolean {
  return workspace.getConfiguration(DEBUG.CONFIG.KEY).get<boolean>(DEBUG.CONFIG.COMPILE_BEFORE_DEBUG, false);
}
