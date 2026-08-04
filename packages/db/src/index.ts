import { drizzle } from "drizzle-orm/node-postgres";
import { Pool } from "pg";
import * as schema from "./schema/index.ts";

export * as schema from "./schema/index.ts";
export * from "./schema/index.ts";

export type Database = ReturnType<typeof createDb>["db"];

export function createDb(connectionString: string, opts: { max?: number } = {}) {
  const pool = new Pool({ connectionString, max: opts.max ?? 10 });
  const db = drizzle(pool, { schema, casing: "snake_case" });
  return { db, pool };
}
