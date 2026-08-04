import { fetchRequestHandler } from "@trpc/server/adapters/fetch";
import { Hono } from "hono";
import { cors } from "hono/cors";
import { logger } from "hono/logger";
import { auth } from "./auth.ts";
import { env } from "./env.ts";
import { createContext } from "./trpc/init.ts";
import { appRouter } from "./trpc/router.ts";

export const app = new Hono();

app.use("*", logger());

// Credentialed CORS: the SPA is served from a different origin in development
// and, behind Caddy, from the same one in production. Both must work.
app.use(
  "/api/*",
  cors({
    origin: (origin) =>
      env.WEB_ORIGIN.includes(origin) || origin === env.PUBLIC_URL ? origin : "",
    credentials: true,
    allowHeaders: ["Content-Type", "Authorization", "x-trpc-source"],
    allowMethods: ["GET", "POST", "OPTIONS"],
  }),
);

app.get("/health", (c) => c.json({ ok: true, name: env.APP_NAME }));

app.on(["GET", "POST"], "/api/auth/*", (c) => auth.handler(c.req.raw));

app.all("/api/trpc/*", (c) =>
  fetchRequestHandler({
    endpoint: "/api/trpc",
    req: c.req.raw,
    router: appRouter,
    createContext,
    onError({ error, path }) {
      if (error.code === "INTERNAL_SERVER_ERROR") {
        console.error(`[trpc] ${path}:`, error.cause ?? error);
      }
    },
  }),
);
