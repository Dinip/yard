import type { z } from "zod";

/**
 * Schemas that should become named Rust types.
 *
 * The zod schema is the source of truth; `named()` is what gives it an identity
 * the Rust emitter can reference. Anything *not* registered gets inlined at its
 * use site, which is fine for primitives and arrays but not for objects — an
 * unregistered nested object has no name to emit, and the generator will say so
 * rather than invent one.
 *
 * Insertion order is emission order, so declare dependencies before dependents
 * to keep the generated file readable.
 */
export const registry = new Map<string, z.ZodType>();
const names = new WeakMap<z.ZodType, string>();

export function named<T extends z.ZodType>(name: string, schema: T): T {
  if (registry.has(name)) throw new Error(`protocol: duplicate schema name "${name}"`);
  registry.set(name, schema);
  names.set(schema, name);
  return schema;
}

export function nameOf(schema: z.ZodType): string | undefined {
  return names.get(schema);
}
