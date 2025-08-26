import { ExtensionContext, extensions, window, workspace } from "vscode";
import { DatabaseController } from "./db/databaseController";
import { API, GitExtension } from "./typings/git";
import { createHash } from "crypto";
import { DynamicObject } from "./typings";
import { AppDataSource } from "./db/datasource";
import { ProjectsFeature } from "./projects/feature";
import { DfmFeature } from "./dfm/feature";

namespace WorkspaceKeys {
  const WORKSPACE_KEY = "projects<%ws>";

  export function get(name: string): string {
    return `${WORKSPACE_KEY}.${name}`;
  }

  export const WATCH_KEYS: string[] = [
    `${WORKSPACE_KEY}.dbChanged`,
  ];
}

export enum RuntimeProperty {
  Workspace,
  WorkspaceAvailable,
}

type RuntimePropertyChangeListener = ((
  property: RuntimeProperty,
  newValue: any,
  oldValue: any
) => void);

/**
 * Runtime class to manage workspace state and global variables.
 *
 * Properties must be synchronously accessible.
 */
export abstract class Runtime {
  private static _listeners: RuntimePropertyChangeListener[] = [];

  private static _workspaceAvailable: boolean = false;
  private static gitAPI?: API;
  private static gitMap: Map<string, string> = new Map();
  public static _workspaceHash: string;
  public static projects: ProjectsFeature;
  public static dfm: DfmFeature;
  public static db: DatabaseController;
  public static extension: ExtensionContext;

  static async initialize(context: ExtensionContext) {
    await AppDataSource.initialize();
    this.db = new DatabaseController();
    this.extension = context;
    this.gitAPI = await this.createGitAPI();
    context.subscriptions.push(
      workspace.onDidChangeWorkspaceFolders(() => {
        this.workspaceAvailable = !!workspace.workspaceFolders?.length;
        this.workspaceHash = this.generateWorkspaceHash();
      })
    );
    this.workspaceHash = this.generateWorkspaceHash();
    this.workspaceAvailable = !!workspace.workspaceFolders?.length;
    this.projects = new ProjectsFeature();
    await this.projects.initialize();
    this.dfm = new DfmFeature();
    await this.dfm.initialize();
    this.watchGitState();
  }

  public static subscribe<T>(
    listener: (property: RuntimeProperty, newValue: T, oldValue: T) => void
  ): void {
    this._listeners.push(listener);
  }

  public static unsubscribe(
    listener: (property: RuntimeProperty, newValue: any, oldValue: any) => void
  ): void {
    this._listeners = this._listeners.filter((l) => l !== listener);
  }

  public static get workspaceAvailable(): boolean {
    return this._workspaceAvailable;
  }

  public static set workspaceAvailable(available: boolean) {
    if (this._workspaceAvailable === available) {
      return;
    }
    this._listeners.forEach((listener) =>
      listener(
        RuntimeProperty.WorkspaceAvailable,
        available,
        this._workspaceAvailable
      )
    );
    this._workspaceAvailable = available;
  }

  public static async assertWorkspaceAvailable(): Promise<boolean> {
    let result = this.workspaceAvailable;
    if (!result) {
      await window.showErrorMessage(
        "No workspace available. Please open a folder or workspace."
      );
    }
    return result;
  }

  public static get workspaceHash(): string {
    return this._workspaceHash;
  }

  /**
   * Sets an application-specific flag in the global state.
   * @param name The name of the flag to set.
   * @param value The value to set for the flag.
   * @returns A promise that resolves when the flag is set.
   */
  public static async setFlag(name: string, value?: DynamicObject, hash?: string): Promise<Thenable<void>> {
    const key = this.getKey(name, hash);
    if (value) {
      return await this.extension.globalState.update(key, JSON.stringify(value));
    } else {
      return await this.extension.globalState.update(key, undefined);
    }
  }

  private static getKey(name: string, hash?: string): string {
    hash = hash || this.workspaceHash;
    return WorkspaceKeys.get(name).replace("%ws", hash);
  }

  /**
   * Retrieves an application-specific flag from the global state.
   * @param name The name of the flag to retrieve.
   * @returns The value of the flag or undefined if not found.
   */
  public static getFlag<T extends DynamicObject>(name: string, hash?: string): T | undefined {
    const value = this.extension.globalState.get<string>(this.getKey(name, hash), "");
    if (value) {
      try {
        return JSON.parse(value) as T;
      } catch (error) {
        console.error(`Failed to parse flag ${name}:`, error);
      }
    }
    return undefined;
  }

  private static set workspaceHash(value: string) {
    if (this._workspaceHash === value) {
      return;
    }
    const old_value = this._workspaceHash;
    this._workspaceHash = value;
    this._listeners.forEach((listener) =>
      listener(RuntimeProperty.Workspace, value, old_value)
    );
  }

  public static get workspaceHashHumanReadable(): string {
    if (workspace.workspaceFolders) {
      const folderString = workspace.workspaceFolders.map(
        (folder) => folder.uri.fsPath
      ).join("+") || "";
      const gitBranchesString = this.gitAPI?.repositories.map(
        r => r.state.HEAD?.name || r.state.HEAD?.commit || ""
      ).join("+") || "none";
      return folderString + '-' + gitBranchesString;
    }
    return "default";
  }

  private static generateWorkspaceHash(): string {
    return createHash("md5").update(this.workspaceHashHumanReadable).digest("hex");
  }

  private static get workspaceWatchKeys(): string[] {
    const hash = this.generateWorkspaceHash();
    return WorkspaceKeys.WATCH_KEYS.map((key) => key.replace("%ws", hash));
  }

  private static async createGitAPI(): Promise<API | undefined> {
    const gitExtension = extensions.getExtension<GitExtension>("vscode.git");
    if (gitExtension) {
      if (!gitExtension.isActive) {
        await gitExtension.activate();
      }
      return gitExtension.exports.getAPI(1);
    }
  }

  private static watchGitState(): void {
    let timeout: NodeJS.Timeout | null = null;
    const sub = this.gitAPI?.onDidOpenRepository((repo) => {
      this.extension.subscriptions.push(
        repo.state.onDidChange(() => {
          if (timeout) {
            clearTimeout(timeout);
          }
          timeout = setTimeout(() => {
            const key = repo.rootUri.fsPath;
            const oldValue = this.gitMap.get(key);
            const newValue = repo.state.HEAD?.name || repo.state.HEAD?.commit || "";
            if (oldValue !== newValue) {
              this.gitMap.set(key, newValue || "");
              this.workspaceHash = this.generateWorkspaceHash();
            }
          }, 2000);
        })
      );
    });
    if (sub) {
      this.extension.subscriptions.push(sub);
    }
  }
}
