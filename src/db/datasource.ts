/**
 * Base DataSource class for managing SQLite database using TypeORM and sql.js.
 */

import { join } from "path";
import { DataSource, EntityManager } from "typeorm";
import { accessSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import initSqlJs from "sql.js";
import { Entities } from "./entities";
import { Runtime } from "../runtime";

const CACHE_DB_FILE_NAME = "vscode.ddk.cachedb";

export class AppDataSource extends DataSource {
  private static sqljs: any;

  public static async initialize(): Promise<void> {
    this.sqljs = await initSqlJs({
      locateFile: (file: string) =>
        Runtime.extension.asAbsolutePath(`dist/${file}`),
    });
  }

  private constructor(dbBuffer?: any) {
    super({
      type: "sqljs",
      driver: AppDataSource.sqljs,
      database: dbBuffer,
      entities: Entities.ALL,
      synchronize: true,
      logging: false,
    });
  }

  public static async transaction(
    callback: (manager: EntityManager) => Promise<void>
  ): Promise<void> {
    const dataSource = new this(this.getReadBuffer());
    await dataSource.initialize();
    await dataSource.manager.transaction(async (manager) => {
      await callback(manager);
    });
    await dataSource.saveToFile();
  }

  public static async readConnection(
    callback: (manager: EntityManager) => Promise<void>
  ): Promise<void> {
    const dataSource = new this(this.getReadBuffer());
    await dataSource.initialize();
    await callback(dataSource.manager);
  }

  private static get cacheDBFilePath(): string {
    const path = Runtime.extension.globalStorageUri.fsPath;
    // Ensure application directory exists
    try {
      accessSync(path);
    } catch {
      mkdirSync(path, { recursive: true });
    }
    return join(path, CACHE_DB_FILE_NAME);
  }

  private static getReadBuffer(): any | undefined {
    try {
      return new Uint8Array(readFileSync(this.cacheDBFilePath));
    } catch {
      return undefined; // New DB if file doesn't exist
    }
  }

  private async saveToFile(reset: boolean = false): Promise<any> {
    const buffer: Uint8Array = reset ? new Uint8Array() : this.sqljsManager.exportDatabase();
    const filePath = AppDataSource.cacheDBFilePath;
    mkdirSync(Runtime.extension.globalStorageUri.fsPath, { recursive: true });
    writeFileSync(filePath, buffer);
  }
}
