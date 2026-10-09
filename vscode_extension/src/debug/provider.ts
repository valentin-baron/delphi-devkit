import { CancellationToken, DebugConfiguration, DebugConfigurationProvider, window, workspace, WorkspaceFolder } from 'vscode';
import { Runtime } from '../runtime';
import { Entities } from '../projects/entities';
import { DEBUG } from '../constants';
import { CompileOutcome, configurationName, linkToCompile, projectReference, projectReferredTo } from './contract';

function allProjects(): Entities.Project[] {
  return Runtime.projectsData?.projects ?? [];
}

export function configurationFor(project: Entities.Project, request: 'launch' | 'attach'): DebugConfiguration {
  return {
    type: DEBUG.TYPE,
    request,
    name: configurationName(request, project, allProjects()),
    ddkProject: projectReference(project, allProjects())
  };
}

/**
 * DDK's projects as dynamic configurations of the `delphi` debug type. Every
 * entry is just `{ type, request, ddkProject }`, which the debugger extension
 * owning the type resolves by asking DDK for the debug target — so nothing
 * debugger-specific is written here and a hand-written launch.json entry looks
 * the same. Unbuilt projects are listed too; a launch builds them first.
 */
export class DdkDebugConfigurationList implements DebugConfigurationProvider {
  async provideDebugConfigurations(_folder: WorkspaceFolder | undefined): Promise<DebugConfiguration[]> {
    return allProjects().flatMap((project) => [configurationFor(project, 'launch'), configurationFor(project, 'attach')]);
  }
}

/**
 * Builds a project before its debug session starts. VS Code asks every provider
 * of a debug type to resolve a configuration however the session was started
 * (context menu, debug dropdown, launch.json, F5), so this is the one place
 * where "compile before debug" holds for all of them. Only launches naming a
 * DDK project are built, and only with `ddk.debug.compileBeforeDebug` on. A
 * build that fails or is cancelled aborts the session: debugging the previous
 * binary against new sources is what the build exists to prevent.
 */
export class DdkBuildBeforeDebug implements DebugConfigurationProvider {
  /** The link the user acted on for the session about to start, if any. */
  private pickedLink?: { project: number; link: number };

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
      // session starts on whatever binary is lying there. A name matching
      // nothing, a name matching several, and a name asked for before the
      // server has sent its projects all land here.
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
   * F5 with no launch.json hands every provider an empty configuration once the
   * `delphi` debugger is picked; left empty, the session dies without a word.
   * DDK's active project is what the user means, and it goes through the build
   * like any other configuration.
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

/** Nothing for a build the user cancelled: the compiler output has said so. */
function whyNotStarted(project: string, outcome: CompileOutcome | undefined): string | undefined {
  if (!outcome)
    return `The DDK server did not report the outcome of compiling "${project}" (is it older than the extension?); the debug session was not started.`;
  if (outcome.cancelled) return undefined;
  return `Compilation of "${project}" failed; the debug session was not started.`;
}

function compileBeforeDebug(): boolean {
  return workspace.getConfiguration(DEBUG.CONFIG.KEY).get<boolean>(DEBUG.CONFIG.COMPILE_BEFORE_DEBUG, false);
}
