import { createDb } from "@farm/db";
import { env } from "./env.ts";

export const { db, pool } = createDb(env.DATABASE_URL);
export type { Database } from "@farm/db";
