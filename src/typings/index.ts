import { ObjectLiteral } from "typeorm";

/**
 * Typing for an object whose properties can be dynamically added.
 */
export type DynamicObject = ObjectLiteral;

export type Coroutine<T, A extends any[] = []> = (...args: A) => Promise<T>;
