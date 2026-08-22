/**
 * Bootstraps the first administrator — there is no other way to get one, since
 * promoting a user is itself an admin-only action.
 *
 *   bun packages/coordinator/src/cli/grant-admin.ts someone@example.com
 */
import { user } from "@yard/db";
import { eq } from "drizzle-orm";
import { db, pool } from "../db.ts";

const email = process.argv[2];
if (!email) {
  console.error("usage: grant-admin <email>");
  process.exit(1);
}

const [updated] = await db
  .update(user)
  .set({ role: "admin" })
  .where(eq(user.email, email))
  .returning({ id: user.id, email: user.email, role: user.role });

await pool.end();

if (!updated) {
  console.error(`no user with email ${email} — sign in once first`);
  process.exit(1);
}
console.log(`${updated.email} is now ${updated.role}`);
