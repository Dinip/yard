import { adminClient } from "better-auth/client/plugins";
import { createAuthClient } from "better-auth/react";

/**
 * Same-origin in production (Caddy proxies /api to the coordinator) and via the
 * vite dev proxy in development — so no absolute baseURL is needed either way.
 */
export const authClient = createAuthClient({
  basePath: "/api/auth",
  plugins: [adminClient()],
});

export const { signIn, signOut, signUp, useSession } = authClient;
