import { app } from "./app.ts";
import { APP_NAME } from "./app-name.ts";
import { db, pool } from "./db.ts";
import { env } from "./env.ts";
import { gatewayWebSocket } from "./gateway/route.ts";
import { runMicrosoftAccountMigration } from "./lib/auth-migration.ts";
import { startReservationReaper } from "./lib/reservations.ts";

try {
  const updated = await runMicrosoftAccountMigration(
    db,
    env.MICROSOFT_CLIENT_ID && env.MICROSOFT_CLIENT_SECRET
      ? { clientId: env.MICROSOFT_CLIENT_ID, tenantId: env.MICROSOFT_TENANT_ID }
      : undefined,
  );
  if (updated) console.log(`[auth] migrated ${updated} Microsoft accounts to directory object IDs`);
} catch (error) {
  console.error(`[auth] ${error instanceof Error ? error.message : "Account migration failed"}`);
  await pool.end();
  process.exit(1);
}

const server = Bun.serve({
  port: env.PORT,
  fetch: app.fetch,
  // Bun needs the socket handlers at the server level; Hono's upgradeWebSocket
  // only arranges the upgrade. Omitting this makes every upgrade silently fail.
  websocket: gatewayWebSocket,
  // Long enough that an idle control-plane socket between heartbeats is not
  // reaped by the server itself.
  idleTimeout: 120,
});

const stopReaper = startReservationReaper(db);

console.log(`[coordinator] ${APP_NAME} listening on :${server.port} (${env.NODE_ENV})`);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    console.log(`[coordinator] ${signal} — shutting down`);
    stopReaper();
    await server.stop();
    process.exit(0);
  });
}
