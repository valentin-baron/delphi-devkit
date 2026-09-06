import { DebugConfiguration, DebugConfigurationProvider, WorkspaceFolder } from 'vscode';
import { Runtime } from '../runtime';
import { Entities } from '../projects/entities';
import { DEBUG } from '../constants';

/**
 * Contributes DDK's projects to the debug dropdown (dynamic configurations)
 * for the `delphi` debug type. Every entry is the two-line form
 * `{ type, request, ddkProject }`: the debugger extension that owns the type
 * resolves it by asking DDK for the project's debug target, so nothing
 * debugger-specific is written here and a hand-written launch.json entry
 * looks exactly the same.
 */
export class DdkDebugConfigurationProvider implements DebugConfigurationProvider {
  /** The configuration DDK starts for a project, launch or attach. */
  public static configurationFor(project: Entities.Project, request: 'launch' | 'attach'): DebugConfiguration {
    const verb = request === 'launch' ? 'Debug' : 'Attach to';
    return {
      type: DEBUG.TYPE,
      request,
      name: `${verb} ${project.name} (DDK)`,
      ddkProject: projectReference(project)
    };
  }

  async provideDebugConfigurations(_folder: WorkspaceFolder | undefined): Promise<DebugConfiguration[]> {
    const runnable = (Runtime.projectsData?.projects ?? []).filter((project) => !!Entities.resolveRunTarget(project));
    return runnable.flatMap((project) => [
      DdkDebugConfigurationProvider.configurationFor(project, 'launch'),
      DdkDebugConfigurationProvider.configurationFor(project, 'attach')
    ]);
  }
}

/**
 * The project's name when it is unique, else its id. Both resolve through
 * `ddk.debug.getDebugTarget`; a name reads better and survives a reset of
 * DDK's project list, an id is unambiguous.
 */
function projectReference(project: Entities.Project): string {
  const sameName = (Runtime.projectsData?.projects ?? []).filter((p) => p.name.toLowerCase() === project.name.toLowerCase());
  return sameName.length === 1 ? project.name : String(project.id);
}
