import { app } from "./app.ts";
import { env } from "./env.ts";
import { gatewayWebSocket } from "./gateway/route.ts";

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

console.log(`[coordinator] ${env.APP_NAME} listening on :${server.port} (${env.NODE_ENV})`);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    console.log(`[coordinator] ${signal} — shutting down`);
    await server.stop();
    process.exit(0);
  });
}
