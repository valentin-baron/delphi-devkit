import { commands, ExtensionContext, workspace } from 'vscode';
import { DatabaseController } from './db/databaseController';
import { AppDataSource } from './db/datasource';
import { ProjectsFeature } from './projects/feature';
import { DfmFeature } from './dfm/feature';
import { Entities } from './db/entities';
import { CompilerConfiguration } from './projects/compiler/compiler';
import { PROJECTS } from './constants';
import { GeneralCommands } from './commands';

/**
 * Runtime class to manage workspace state and global variables.
 *
 * Properties must be synchronously accessible.
 */
export abstract class Runtime {
  public static projects: ProjectsFeature;
  public static dfm: DfmFeature;
  public static db: DatabaseController;
  public static _configEntity: Entities.Configuration;
  public static extension: ExtensionContext;

  public static async refreshConfigEntity(): Promise<void> {
    this._configEntity = await this.db.getConfiguration();
  }

  public static get configEntity(): Entities.Configuration {
    while (!this._configEntity) {
      const view = new Int32Array(new SharedArrayBuffer(4));
      Atomics.wait(view, 0, 0, 50);
    }
    return this._configEntity;
  }

  static async initialize(context: ExtensionContext) {
    this.extension = context;
    await AppDataSource.initialize();
    this.db = new DatabaseController();
    await this.refreshConfigEntity();
    this.projects = new ProjectsFeature();
    await this.projects.initialize();
    this.dfm = new DfmFeature();
    await this.dfm.initialize();
    context.subscriptions.push(
      ...GeneralCommands.registers
    );
  }

  public static setContext(name: string, value: any): Thenable<void> {
    return commands.executeCommand('setContext', name, value);
  }

  public static get compilerConfigurations(): CompilerConfiguration[] {
    const config = workspace.getConfiguration(PROJECTS.CONFIG.KEY);
    return config.get<CompilerConfiguration[]>(PROJECTS.CONFIG.COMPILER.CONFIGURATIONS, []);
  }
}
