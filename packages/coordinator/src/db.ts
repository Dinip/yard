import { createDb } from "@yard/db";
import { env } from "./env.ts";

export const { db, pool } = createDb(env.DATABASE_URL);
export type { Database } from "@yard/db";
