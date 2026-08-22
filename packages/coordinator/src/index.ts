import { app } from "./app.ts";
import { APP_NAME } from "./app-name.ts";
import { db } from "./db.ts";
import { env } from "./env.ts";
import { gatewayWebSocket } from "./gateway/route.ts";
import { startReservationReaper } from "./lib/reservations.ts";

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
