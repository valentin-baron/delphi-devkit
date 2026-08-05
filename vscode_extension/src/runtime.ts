import { commands, ExtensionContext, OutputChannel, window } from 'vscode';
import { ProjectsFeature } from './projects/feature';
import { DfmFeature } from './dfm/feature';
import { Entities } from './projects/entities';
import { GeneralCommands } from './commands';
import { DDK_Client } from './client';
import { PROJECTS } from './constants';
import { randomUUID, UUID } from 'crypto';
import { Option } from './types';
import { McpServerFeature } from './mcp/server';
import { DelphiLspFeature } from './delphilsp/feature';

/**
 * Runtime class to manage workspace state and global variables.
 *
 * Properties must be synchronously accessible.
 */
export abstract class Runtime {
  private static _events: string[] = [];
  private static _failedEvents: string[] = [];

  public static projectsData: Entities.ProjectsData;
  public static compilerConfigurations: Entities.CompilerConfigurations;
  public static projects: ProjectsFeature;
  public static dfm: DfmFeature;
  public static extension: ExtensionContext;
  public static client: DDK_Client;
  public static compilerOutputChannel: OutputChannel;
  /** stdout/stderr of project executables started through Run. */
  public static runOutputChannel: OutputChannel;
  public static mcp: McpServerFeature;
  public static delphilsp: DelphiLspFeature;

  static async initialize(context: ExtensionContext) {
    this.extension = context;
    this.compilerOutputChannel = window.createOutputChannel('DDK Compiler', 'ddk.compiler');
    this.runOutputChannel = window.createOutputChannel('DDK Run');
    // Initialized before the client so its availability flag and hook are
    // ready by the time the client's own initial `refresh()` runs.
    this.delphilsp = new DelphiLspFeature();
    await this.delphilsp.initialize();
    this.client = new DDK_Client();
    await this.client.initialize();
    this.projects = new ProjectsFeature();
    await this.projects.initialize();
    this.dfm = new DfmFeature();
    await this.dfm.initialize();
    // Register the MCP server (spawns ddk-mcp-server as a STDIO child process
    // when VS Code or another MCP client requests it).
    this.mcp = new McpServerFeature();
    await this.mcp.initialize();
    context.subscriptions.push(
      ...GeneralCommands.registers,
      this.compilerOutputChannel,
      this.runOutputChannel
    );
  }

  public static get activeProject(): Option<Entities.Project> {
    return this.projectsData.projects.find((p) => p.id === this.projectsData.active_project_id);
  }

  /** Update VS Code context keys that govern keybinding `when` clauses.
   *  Must be called whenever `projectsData` changes so that shortcuts
   *  work even when the tree view is not visible. */
  public static updateProjectContexts(): void {
    const hasSelected = !!this.projectsData?.active_project_id;
    const active = this.activeProject;
    // A configured Host Application makes a project runnable even without an
    // own executable (e.g. a .dpk package run through its hosting exe).
    const hasRunTarget = !!(active && Entities.resolveRunTarget(active));
    this.setContext(PROJECTS.CONTEXT.IS_PROJECT_SELECTED, hasSelected);
    this.setContext(PROJECTS.CONTEXT.DOES_SELECTED_PROJECT_HAVE_EXE, hasRunTarget);
  }

  public static get groupProjectsCompiler(): Option<Entities.CompilerConfiguration> {
    if (!this.projectsData.group_project_compiler_id) return undefined;
    return this.compilerConfigurations?.[this.projectsData.group_project_compiler_id];
  }

  public static getProjectOfLink(link: Entities.ProjectLink): Option<Entities.Project> {
    return this.projectsData?.projects.find((p) => p.id === link.project_id);
  }

  public static getWorkspaceOfLink(link: Entities.ProjectLink): Option<Entities.Workspace> {
    return this.projectsData?.workspaces.find((ws) => ws.project_links.some((l) => link.id === l.id));
  }

  public static getLinksOfProject(project?: Option<Entities.Project>): Entities.ProjectLink[] {
    if (!project) return [];
    const workspaceLinks = Runtime.projectsData?.workspaces
      .flatMap((ws) => ws.project_links)
      .filter((link) => link.project_id === project.id) || [];
    const groupProjectLinks = Runtime.projectsData?.group_project?.project_links.filter(
      (link) => link.project_id === project.id
    ) || [];
    return [...workspaceLinks, ...groupProjectLinks];
  }

  public static async compileProjectLink(link: Entities.ProjectLink, recreate: boolean = false): Promise<boolean> {
    return await this.client.compileProject(recreate, link.project_id, link.id);
  }

  public static setContext(name: string, value: any): Thenable<void> {
    return commands.executeCommand('setContext', name, value);
  }

  public static addEvent(timeout: number = 5000): UUID {
    const id = randomUUID();
    this._events.push(id);
    if (timeout > 0)
      setTimeout(() => {
        if (!this._events.includes(id)) return;
        setTimeout(() => this._failedEvents = this._failedEvents.filter((it) => it !== id), 60000);
        this._failedEvents.push(id);
        this.finishEvent(id);
        window.showErrorMessage(`Server Operation timed out.`);
      }, timeout);

    return id;
  }

  public static finishEvent(id: string): void {
    this._events = this._events.filter((it) => it !== id);
    this._failedEvents = this._failedEvents.filter((it) => it !== id);
  }

  public static async waitForEvent(id: string): Promise<boolean> {
    while (this._events.includes(id)) await new Promise((resolve) => setTimeout(resolve, 100));
    return !this._failedEvents.includes(id);
  }
}
