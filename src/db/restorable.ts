import { FindOneOptions } from "typeorm";
import { Class, Coroutine, DynamicObject } from "../typings";
import { AppDataSource } from "./datasource";

export abstract class Restorable<T extends DynamicObject> {
  private static items: Map<any, Set<Restorable<any>>> = new Map();
  constructor(private entityClass: Class<T>) {
    Restorable.register(this.entityClass, this);
  }

  public abstract loadOptions(): FindOneOptions<T>;
  public abstract restore(entity: T): Promise<void>;
  public createCallback(): Coroutine<T> | undefined {
    return undefined;
  }

  public static async restore(): Promise<void> {
    await Promise.all(
      Array.from(this.items.values()).map(
        async (restorables) => {
          await Promise.all(Array.from(restorables).map(async r => {
            const entity = await AppDataSource.load(r.entityClass, r.loadOptions(), r.createCallback());
            await r.restore(entity);
          }));
        }
      )
    );
  }

  private static register<T extends DynamicObject>(classType: Class<T>, item: Restorable<T>): void {
    if (!this.items.has(classType)) {
      this.items.set(classType, new Set());
    }
    this.items.get(classType)!.add(item);
  }
}