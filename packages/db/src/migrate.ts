import { migrate } from "drizzle-orm/node-postgres/migrator";
import { createDb } from "./index.ts";

const url = process.env.DATABASE_URL;
if (!url) {
  console.error("DATABASE_URL is required");
  process.exit(1);
}

// In the container the SQL is copied next to the bundle, not two levels up from
// source, so the path is overridable.
const migrationsFolder =
  process.env.MIGRATIONS_DIR ?? new URL("../drizzle", import.meta.url).pathname;

const { db, pool } = createDb(url, { max: 1 });
await migrate(db, { migrationsFolder });
await pool.end();
console.log(`[db] migrations applied from ${migrationsFolder}`);
