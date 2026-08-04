import { app } from "./app.ts";
import { env } from "./env.ts";

const server = Bun.serve({
  port: env.PORT,
  fetch: app.fetch,
  idleTimeout: 60,
});

console.log(`[coordinator] ${env.APP_NAME} listening on :${server.port} (${env.NODE_ENV})`);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    console.log(`[coordinator] ${signal} — shutting down`);
    await server.stop();
    process.exit(0);
  });
}
