import { join } from "path";
import { DataSource, FindOneOptions } from "typeorm";
import { accessSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import initSqlJs from "sql.js";
import { WorkspaceEntity, GroupProjectEntity, ProjectEntity } from "./entities";
import { Runtime } from "../runtime";
import { Class, DynamicObject } from "../typings";

export class AppDataSource extends DataSource {
  private static sqljs: any;

  public static async initialize(): Promise<void> {
    this.sqljs = await initSqlJs({
      locateFile: (file: string) =>
        Runtime.extension.asAbsolutePath(`dist/${file}`),
    });
  }

  public static async load<T extends DynamicObject>(
    entityClass: Class<T>,
    opt: FindOneOptions<T>,
    createCallback?: () => Promise<T>
  ): Promise<T | null> {
    const dataSource = new this(await this.getReadBuffer());
    await dataSource.initialize();
    const entity = await dataSource.getRepository(entityClass).findOne(opt);
    if (entity) {
      return entity;
    }
    if (createCallback) {
      const newEntity = await createCallback();
      const entity = await dataSource
        .getRepository(entityClass)
        .save(newEntity, { transaction: true });
      await dataSource.saveToFile();
      return entity;
    }
    return null;
  }

  public static async save<T extends DynamicObject>(
    entityClass: Class<T>,
    opt: FindOneOptions<T>,
    modifyCallback: (ent: T) => Promise<T | unknown>,
    createCallback?: () => Promise<T>
  ): Promise<T | null> {
    const dataSource = new this(await this.getReadBuffer());
    await dataSource.initialize();
    let entity = await dataSource.getRepository(entityClass).findOne(opt);
    if (!entity) {
      if (createCallback) {
        const newEntity = await createCallback() as T;
        entity = await dataSource
          .getRepository(entityClass)
          .save(newEntity, { transaction: true });
      } else {
        return null;
      }
    }
    let modifiedEntity = await modifyCallback(entity);
    if (!(modifiedEntity instanceof entityClass)) {
      modifiedEntity = entity; // so we can just modify without returning a new instance
    }
    await dataSource
      .getRepository(entityClass)
      .save(modifiedEntity as T, { transaction: true });
    await dataSource.saveToFile();
    await Runtime.setFlag("dbChanged", { changeType: "update" });
    return modifiedEntity as T;
  }

  public static async reset(): Promise<void> {
    const dataSource = new this(new Uint8Array());
    await dataSource.saveToFile(true);
    await Runtime.setFlag("dbChanged", { changeType: "drop" });
  }

  private constructor(dbBuffer?: any) {
    super({
      type: "sqljs",
      driver: AppDataSource.sqljs,
      database: dbBuffer,
      entities: [WorkspaceEntity, GroupProjectEntity, ProjectEntity],
      synchronize: true,
      logging: false,
    });
  }

  private static get workspaceDbFileName(): string {
    return `${Runtime.workspaceHash}.cachedb`;
  }

  private static get cacheDBFilePath(): string {
    const path = Runtime.extension.globalStorageUri.fsPath;
    // Ensure application directory exists
    try {
      accessSync(path);
    } catch {
      mkdirSync(path, { recursive: true });
    }
    return join(path, this.workspaceDbFileName);
  }

  private static async getReadBuffer(): Promise<any | undefined> {
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
