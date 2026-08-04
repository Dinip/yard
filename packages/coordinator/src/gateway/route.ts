import type { CoordinatorMessage } from "@farm/protocol";
import type { ServerWebSocket } from "bun";
import { Hono } from "hono";
import { createBunWebSocket } from "hono/bun";
import { bearerFrom } from "../lib/provider-token.ts";
import { type AuthedProvider, authenticateProvider, GatewaySession } from "./handler.ts";

const { upgradeWebSocket, websocket } = createBunWebSocket<ServerWebSocket>();

/** Must be handed to `Bun.serve({ websocket })` — see src/index.ts. */
export const gatewayWebSocket = websocket;

/** The auth middleware resolves the credential once and the upgrade reuses it. */
type GatewayEnv = { Variables: { providerAuth: AuthedProvider } };

export const gatewayRoutes = new Hono<GatewayEnv>();

/**
 * The provider control plane. Providers dial *out* to this, so a provider needs
 * no inbound reachability and works the same on-box or behind a NAT.
 *
 * Authentication happens before the upgrade: an unauthenticated caller gets a
 * 401 rather than a socket that is immediately closed, which is much easier to
 * diagnose from a provider log.
 */
gatewayRoutes.get(
  "/api/providers/connect",
  async (c, next) => {
    const token = bearerFrom(c.req.raw.headers);
    if (!token) return c.json({ error: "missing bearer token" }, 401);

    const auth = await authenticateProvider(token);
    if (!auth) return c.json({ error: "invalid or revoked provider token" }, 401);

    c.set("providerAuth", auth);
    await next();
  },
  upgradeWebSocket((c) => {
    const auth = c.get("providerAuth");
    let session: GatewaySession | null = null;

    return {
      onOpen(_event, ws) {
        const send = (msg: CoordinatorMessage) => ws.send(JSON.stringify(msg));
        const close = (code: number, reason: string) => ws.close(code, reason);
        session = new GatewaySession(auth, send, close);
      },
      onMessage(event) {
        // Control-plane frames are always JSON text. Binary here means a
        // provider is confusing this socket with the session plane.
        if (typeof event.data !== "string") return;
        void session?.onMessage(event.data);
      },
      onClose() {
        void session?.onClose();
        session = null;
      },
      onError(err) {
        console.error(`[gateway] ${auth.providerId}: socket error:`, err);
      },
    };
  }),
);
