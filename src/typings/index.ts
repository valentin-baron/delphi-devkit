import { ObjectLiteral } from "typeorm";

/**
 * Typing for an object whose properties can be dynamically added.
 */
export type DynamicObject = ObjectLiteral;

export type Class<T = any> = { new (...args: any[]): T };

export type Coroutine<T, A extends any[] = []> = (...args: A) => Promise<T>;