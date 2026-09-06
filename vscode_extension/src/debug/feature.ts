import { commands, debug, DebugConfigurationProviderTriggerKind, Disposable, extensions, window, workspace } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { DEBUG } from '../constants';
import { Entities } from '../projects/entities';
import { BaseFileItem } from '../projects/trees/items/baseFile';
import { DdkDebugConfigurationProvider } from './provider';

/**
 * Debugging a DDK project with whichever debugger registers the `delphi`
 * debug type.
 *
 * DDK owns the gesture and the knowledge: **Debug** / **Attach Debugger** on
 * a project (context menu, command palette, keybinding), one dynamic entry
 * per project in the debug dropdown, and the `ddk.debug.getDebugTarget`
 * command through which another extension obtains the project's debug
 * target (`debug/target`: executable or host, symbols, modules, sources,
 * arguments, warnings). The debugger extension owns the session: it resolves
 * `{ type: 'delphi', request, ddkProject }` by calling that command and
 * fills in its own launch attributes. DDK never writes a launch.json and
 * knows no debugger's configuration format.
 *
 * The commands and menu items exist only while an extension contributing
 * the `delphi` debug type is installed (`ddk:debuggerAvailable`, kept
 * current when extensions change); the target query is always registered.
 */
export class DebugFeature implements Feature {
  private available = false;
  private registrations: Disposable[] = [];

  /** An extension contributing the `delphi` debug type is installed. */
  public get isDebuggerAvailable(): boolean {
    return this.available;
  }

  public async initialize(): Promise<void> {
    Runtime.extension.subscriptions.push(
      commands.registerCommand(DEBUG.COMMAND.GET_DEBUG_TARGET, (args?: { project?: string; compiler?: string }) =>
        Runtime.client.debugTarget(args?.project, args?.compiler)),
      extensions.onDidChange(() => this.updateAvailability()),
      { dispose: () => this.unregister() }
    );
    this.updateAvailability();
  }

  private updateAvailability(): void {
    const available = extensions.all.some((extension) => contributesDelphiDebugger(extension.packageJSON));
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
        new DdkDebugConfigurationProvider(),
        DebugConfigurationProviderTriggerKind.Dynamic
      ),
      commands.registerCommand(DEBUG.COMMAND.DEBUG_PROJECT, (item: BaseFileItem) => this.startSession(item.project.entity, 'launch')),
      commands.registerCommand(DEBUG.COMMAND.ATTACH_PROJECT, (item: BaseFileItem) => this.startSession(item.project.entity, 'attach')),
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
   * One gesture, like the Delphi IDE's Run-with-debugger: for a launch,
   * first an incremental compile with the full debug artefact set (unless
   * `ddk.debug.compileBeforeDebug` is off), then the session. Attaching
   * never compiles: the process is already running.
   */
  private async startSession(project: Entities.Project, request: 'launch' | 'attach'): Promise<void> {
    if (request === 'launch' && compileBeforeDebug()) {
      const link = Runtime.getLinksOfProject(project)[0];
      if (link && !(await Runtime.compileProjectLink(link, false, true))) {
        window.showErrorMessage(`Compilation of "${project.name}" failed; the debug session was not started.`);
        return;
      }
    }
    await debug.startDebugging(undefined, DdkDebugConfigurationProvider.configurationFor(project, request));
  }
}

function compileBeforeDebug(): boolean {
  return workspace.getConfiguration(DEBUG.CONFIG.KEY).get<boolean>(DEBUG.CONFIG.COMPILE_BEFORE_DEBUG, true);
}

function contributesDelphiDebugger(packageJson: { contributes?: { debuggers?: { type?: string }[] } } | undefined): boolean {
  const debuggers = packageJson?.contributes?.debuggers ?? [];
  return debuggers.some((contribution) => contribution.type === DEBUG.TYPE);
}
