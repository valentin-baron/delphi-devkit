import { ExtensionContext, extensions, window, workspace } from "vscode";
import { DelphiProjectsProvider } from "./projects/treeItems/provider";
import { Compiler } from "./projects/compiler/compiler";
import { CompilerPicker } from "./projects/contextMenu/statusBar";
import { DatabaseController } from "./db/databaseController";
import { API, GitExtension } from "./typings/git";
import { createHash } from "crypto";
import { v4 as uuid } from "uuid";
import { DynamicObject, LockInfo } from "./typings";
import { AppDataSource } from "./db/datasource";

namespace WorkspaceKeys {
  const WORKSPACE_KEY = "projects<%ws>";

  export function get(name: string): string {
    return `${WORKSPACE_KEY}.${name}`;
  }

  export const WATCH_KEYS: string[] = [
    `${WORKSPACE_KEY}.dbChanged`,
  ];
}

export interface GlobalStateChangeObject {
  key: string;
  windowID: string; // windowID
}

export enum RuntimeProperty {
  Workspace,
  WorkspaceAvailable,
  GlobalState,
}

/**
 * Wrapper for all Statusbar components
 */
class StatusBar {
  constructor(public compilerPicker: CompilerPicker) {}
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

  private static locks: string[] = [];
  private static _workspaceAvailable: boolean = false;
  private static gitAPI?: API;
  private static gitMap: Map<string, string> = new Map();
  public static _workspaceHash: string;
  public static projectsProvider: DelphiProjectsProvider;
  public static db: DatabaseController;
  public static compiler: Compiler;
  public static statusBar: StatusBar;
  public static extension: ExtensionContext;
  public static readonly windowID: string = uuid();

  static async initialize(context: ExtensionContext) {
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
    this.projectsProvider = new DelphiProjectsProvider();
    await AppDataSource.initialize();
    this.db = new DatabaseController();
    this.compiler = new Compiler();
    this.statusBar = new StatusBar(new CompilerPicker());
    this.watchGlobalState();
    this.watchGitState();
  }

  public static async finalize() {
    await Promise.all(this.locks.map(async (l) => await this.extension.globalState.update(l, undefined)));
    this.locks = [];
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
   * Executes a callback with a global lock.
   * @param name The name of the lock to acquire.
   * @param callback The callback to execute with the lock.
   * @returns The result of the callback.
   */
  public static async withLock<T>(name: string, callback: () => Promise<T | null>): Promise<T | null> {
    const lock = await this.acquireLock(name);
    try {
      return await callback();
    } finally {
      await this.releaseLock(name, lock);
    }
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
      value.windowID = this.windowID;
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

  /**
   * Acquires a lock for the specified workspace.
   * @param name The name of the key a lock for.
   * @returns The workspace hash for the locked state.
   */
  private static async acquireLock(name: string): Promise<string> {
    const lockHash = Runtime.workspaceHash;
    let lockInfo = this.getFlag<LockInfo>(name, lockHash);
    const start = Date.now();
    // The lock is by this window => no need to wait
    // The lock is by another window and the counter is 0 => we can take the lock
    while (lockInfo?.windowID !== this.windowID && (lockInfo?.counter || 0) > 0) {
      if (lockInfo && Date.now() - start > 5000) {
        lockInfo.counter = 0;
        break; // Avoid infinite waiting. The lock can't be closed for so long.
      }
      await new Promise(resolve => setTimeout(resolve, 100));
      lockInfo = this.getFlag<LockInfo>(name, lockHash);
    }
    await this.setFlag(name, { counter: (lockInfo?.counter || 0) + 1 }, lockHash);
    this.locks.push(name);
    return lockHash;
  }

  /**
   * Releases the lock for the specified workspace.
   * @param name The name of the key to release the lock for.
   * @param lockHash The hash of the workspace.
   */
  private static async releaseLock(name: string, lockHash: string): Promise<void> {
    let lockInfo = this.getFlag<LockInfo>(name, lockHash);
    if (!!lockInfo && lockInfo.counter > 1) {
      await this.setFlag(name, { counter: lockInfo.counter - 1 }, lockHash);
    } else {
      this.locks = this.locks.filter((l) => l !== name);
      await this.setFlag(name, undefined, lockHash);
    }
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

  /**
   * Periodically checks keys within extension.globalState for changes.
   */
  private static watchGlobalState(): void {
    let previousValues: string[] = this.workspaceWatchKeys.map((key) =>
      this.extension.globalState.get<string>(key, "")
    );
    const iv = setInterval(() => {
      const keys = this.workspaceWatchKeys;
      const currentValues: string[] = keys.map((key) => this.extension.globalState.get<string>(key, ""));
      currentValues.forEach((value, index) => {
        if (value !== previousValues[index]) {
          this._listeners.forEach((listener) =>
            listener(
              RuntimeProperty.GlobalState,
              {
                key: keys[index],
                windowID: value,
              } as GlobalStateChangeObject,
              {
                key: keys[index],
                windowID: previousValues[index],
              } as GlobalStateChangeObject
            )
          );
        }
      });
    }, 2000);

    this.extension.subscriptions.push({
      dispose: () => clearInterval(iv),
    });
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
