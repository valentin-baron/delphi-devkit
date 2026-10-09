import { commands, debug, DebugConfigurationProviderTriggerKind, Disposable, extensions, window } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { DEBUG } from '../constants';
import { Entities } from '../projects/entities';
import { BaseFileItem } from '../projects/trees/items/baseFile';
import { configurationFor, DdkBuildBeforeDebug, DdkDebugConfigurationList } from './provider';
import { contributesDebugger } from './contract';

/** The arguments of `ddk.debug.getDebugTarget`, all optional. */
export interface DebugTargetRequest {
  /** A project id, name or project file; the active project when omitted. */
  project?: string;
  /** The compiler of a project that belongs to no workspace. */
  compiler?: string;
  /** Describe this configuration instead of the project's active one. */
  config?: string;
  /** Describe this platform instead of the project's active one. */
  platform?: string;
}

/**
 * Debugging a DDK project with whichever debugger registers the `delphi` debug
 * type. DDK owns the gesture and the knowledge: the Debug/Attach commands, the
 * dynamic dropdown entries, the build preceding a launch, and the
 * `ddk.debug.getDebugTarget` command another extension calls for the target.
 * The debugger extension owns the session and fills in its own launch
 * attributes; DDK never writes a launch.json and knows no debugger's format.
 *
 * Commands and menu items are enabled only while such an extension is
 * installed (`ddk:debuggerAvailable`); the target query is always registered.
 */
export class DebugFeature implements Feature {
  private available = false;
  private registrations: Disposable[] = [];
  private readonly buildBeforeDebug = new DdkBuildBeforeDebug();

  public get isDebuggerAvailable(): boolean {
    return this.available;
  }

  public async initialize(): Promise<void> {
    Runtime.extension.subscriptions.push(
      commands.registerCommand(DEBUG.COMMAND.GET_DEBUG_TARGET, (request?: DebugTargetRequest) =>
        Runtime.client.debugTarget(request?.project, request?.compiler, request?.config, request?.platform)
      ),
      extensions.onDidChange(() => this.updateAvailability()),
      { dispose: () => this.unregister() }
    );
    this.updateAvailability();
  }

  private updateAvailability(): void {
    const available = extensions.all.some((extension) => contributesDebugger(extension.packageJSON, DEBUG.TYPE));
    if (available === this.available) return;
    this.available = available;
    Runtime.setContext(DEBUG.CONTEXT.AVAILABLE, available);
    if (available) this.register();
    else this.unregister();
  }

  private register(): void {
    this.registrations = [
      debug.registerDebugConfigurationProvider(
        DEBUG.TYPE,
        new DdkDebugConfigurationList(),
        DebugConfigurationProviderTriggerKind.Dynamic
      ),
      debug.registerDebugConfigurationProvider(DEBUG.TYPE, this.buildBeforeDebug),
      commands.registerCommand(DEBUG.COMMAND.DEBUG_PROJECT, (item: BaseFileItem) =>
        this.startSession(item.project.entity, 'launch', item.project.link)
      ),
      commands.registerCommand(DEBUG.COMMAND.ATTACH_PROJECT, (item: BaseFileItem) =>
        this.startSession(item.project.entity, 'attach', item.project.link)
      ),
      commands.registerCommand(DEBUG.COMMAND.DEBUG_SELECTED_PROJECT, () => this.startSelected('launch')),
      commands.registerCommand(DEBUG.COMMAND.ATTACH_SELECTED_PROJECT, () => this.startSelected('attach'))
    ];
  }

  private unregister(): void {
    for (const registration of this.registrations) registration.dispose();
    this.registrations = [];
  }

  private async startSelected(request: 'launch' | 'attach'): Promise<void> {
    const project = Runtime.activeProject;
    if (!project) {
      window.showWarningMessage('No project selected to debug.');
      return;
    }
    await this.startSession(project, request);
  }

  /**
   * Only asks for the session; the build happens in `DdkBuildBeforeDebug`,
   * where every session passes, which is told the link this one was asked on.
   */
  private async startSession(project: Entities.Project, request: 'launch' | 'attach', link?: Entities.ProjectLink): Promise<void> {
    this.buildBeforeDebug.pick(project, link);
    await debug.startDebugging(undefined, configurationFor(project, request));
  }
}
