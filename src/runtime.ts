import { commands, ExtensionContext } from "vscode";
import { DatabaseController } from "./db/databaseController";
import { AppDataSource } from "./db/datasource";
import { ProjectsFeature } from "./projects/feature";
import { DfmFeature } from "./dfm/feature";

/**
 * Runtime class to manage workspace state and global variables.
 *
 * Properties must be synchronously accessible.
 */
export abstract class Runtime {
  public static projects: ProjectsFeature;
  public static dfm: DfmFeature;
  public static db: DatabaseController;
  public static extension: ExtensionContext;

  static async initialize(context: ExtensionContext) {
    this.extension = context;
    await AppDataSource.initialize();
    this.db = new DatabaseController();
    this.projects = new ProjectsFeature();
    await this.projects.initialize();
    this.dfm = new DfmFeature();
    await this.dfm.initialize();
  }

  public static setContext(name: string, value: any): Thenable<void> {
    return commands.executeCommand('setContext', name, value);
  }
}
