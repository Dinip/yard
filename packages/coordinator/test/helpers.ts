import { db, pool } from "../src/db.ts";
import { ProviderConnection, providers } from "../src/gateway/registry.ts";
import { appRouter } from "../src/trpc/router.ts";

/**
 * `src/db.ts` exports one process-wide pool, and `bun test` runs every file in
 * the same process. If each file ended the pool in its own `afterAll`, the first
 * to finish would break the rest — so teardown happens once, on process exit.
 */
let registered = false;
export function closePoolOnExit() {
  if (registered) return;
  registered = true;
  process.on("exit", () => {
    void pool.end();
  });
}

/** A tRPC caller with a synthetic session, bypassing better-auth. */
export function caller(userId: string, role: "user" | "admin" = "user") {
  return appRouter.createCaller({
    db,
    req: new Request("http://test.local"),
    session: { id: `sess-${userId}`, userId } as never,
    user: { id: userId, role, banned: false } as never,
  });
}

/**
 * Registers a loopback provider connection: every command it is sent is
 * answered `ok` immediately.
 *
 * `device.reserve` refuses a device whose provider has no live socket, so tests
 * that are about database behaviour rather than the control plane still need a
 * connection present. Tests that care what was *sent* should use the real
 * FakeProvider over a real socket instead — see gateway.test.ts.
 */
export function stubProviderConnection(providerId: string, publicBaseUrl = "https://stub.test") {
  const conn: ProviderConnection = new ProviderConnection(providerId, publicBaseUrl, (msg) => {
    if (msg.type === "command") {
      queueMicrotask(() => conn.settle(msg.id, true, undefined));
    }
  });
  providers.add(conn);
  return conn;
}

/** Shape of a user row that satisfies the not-null columns. */
export function testUser(id: string) {
  return {
    id,
    name: id,
    email: `${id}@test.local`,
    emailVerified: true,
    createdAt: new Date(),
    updatedAt: new Date(),
  };
}
