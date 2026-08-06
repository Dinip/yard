/**
 * End-to-end control plane, exercised with the fake provider.
 *
 * Boots the real Hono app on a port, connects a real WebSocket, and asserts
 * against the real database — the state machine's whole job is coordinating
 * those three, so mocking any of them would test nothing.
 *
 * Requires Postgres at DATABASE_URL:
 *   docker compose -f docker-compose.dev.yml up -d
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { device, provider, providerToken, reservation, user } from "@farm/db";
import { FakeProvider, makeDevices } from "@farm/protocol/test/fake-provider";
import { eq, inArray } from "drizzle-orm";
import { app } from "../src/app.ts";
import { db } from "../src/db.ts";
import { gatewayWebSocket } from "../src/gateway/route.ts";
import { generateProviderToken } from "../src/lib/provider-token.ts";
import { caller, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();

const PROVIDER_ID = "gw-test-provider";
const OTHER_PROVIDER_ID = "gw-test-provider-2";
const USER_A = "gw-test-user-a";
const USER_B = "gw-test-user-b";
const PUBLIC_BASE = "https://gw-test.example.com";

let server: ReturnType<typeof Bun.serve>;
let baseUrl: string;
let token: string;
let devices: ReturnType<typeof makeDevices>;

beforeAll(async () => {
  await cleanup();

  server = Bun.serve({ port: 0, fetch: app.fetch, websocket: gatewayWebSocket });
  baseUrl = `http://localhost:${server.port}`;

  await db.insert(user).values([USER_A, USER_B].map(testUser));

  await db.insert(provider).values([
    { id: PROVIDER_ID, name: "gw-test", publicBaseUrl: PUBLIC_BASE, status: "offline" },
    {
      id: OTHER_PROVIDER_ID,
      name: "gw-test-2",
      publicBaseUrl: "https://other.example.com",
      status: "offline",
    },
  ]);

  const generated = generateProviderToken();
  token = generated.plaintext;
  await db.insert(providerToken).values({
    id: crypto.randomUUID(),
    providerId: PROVIDER_ID,
    tokenHash: generated.hash,
    label: "test",
  });

  devices = makeDevices(3);
});

afterAll(async () => {
  server?.stop(true);
  await cleanup();
});

async function cleanup() {
  const owned = await db
    .select({ id: device.id })
    .from(device)
    .where(inArray(device.providerId, [PROVIDER_ID, OTHER_PROVIDER_ID]));
  const ids = owned.map((d) => d.id);
  if (ids.length) await db.delete(reservation).where(inArray(reservation.deviceId, ids));
  await db.delete(device).where(inArray(device.providerId, [PROVIDER_ID, OTHER_PROVIDER_ID]));
  await db.delete(providerToken).where(eq(providerToken.providerId, PROVIDER_ID));
  await db.delete(provider).where(inArray(provider.id, [PROVIDER_ID, OTHER_PROVIDER_ID]));
  await db.delete(user).where(inArray(user.id, [USER_A, USER_B]));
}

function connectFake(overrides: Partial<ConstructorParameters<typeof FakeProvider>[0]> = {}) {
  return new FakeProvider({
    url: baseUrl,
    token,
    providerId: PROVIDER_ID,
    name: "gw-test",
    publicBaseUrl: PUBLIC_BASE,
    devices,
    ...overrides,
  });
}

describe("provider gateway", () => {
  test("rejects a connection with no bearer token", async () => {
    const res = await fetch(`${baseUrl}/api/providers/connect`, {
      headers: { Upgrade: "websocket", Connection: "Upgrade" },
    });
    expect(res.status).toBe(401);
  });

  test("rejects an unknown token", async () => {
    const fake = connectFake({ token: "pft_definitely-not-a-real-token" });
    await expect(fake.connect()).rejects.toThrow();
    await fake.close();
  });

  test("rejects a token whose provider id does not match the hello", async () => {
    const fake = connectFake({ providerId: OTHER_PROVIDER_ID });
    await expect(fake.connect()).rejects.toThrow();
    await fake.close();
  });

  test("hello registers the provider and its whole inventory", async () => {
    const fake = connectFake();
    await fake.connect();

    const [row] = await db.select().from(provider).where(eq(provider.id, PROVIDER_ID));
    expect(row?.status).toBe("online");
    expect(row?.version).toBe("fake-0.1.0");
    expect(row?.publicBaseUrl).toBe(PUBLIC_BASE);

    const stored = await db.select().from(device).where(eq(device.providerId, PROVIDER_ID));
    expect(stored).toHaveLength(devices.length);
    expect(stored.every((d) => d.status === "ready")).toBe(true);

    const ios = stored.find((d) => d.platform === "ios");
    expect(ios?.displayWidth).toBe(1179);
    expect(ios?.streamCodec).toBe("hev1.1.6.L93.B0");

    await fake.close();
  });

  test("reconnecting with a smaller inventory marks the missing devices absent", async () => {
    const fake = connectFake({ devices: devices.slice(0, 1) });
    await fake.connect();

    const stored = await db.select().from(device).where(eq(device.providerId, PROVIDER_ID));
    const absent = stored.filter((d) => d.status === "absent");
    expect(absent).toHaveLength(devices.length - 1);
    expect(stored.find((d) => d.id === devices[0]!.id)?.status).toBe("ready");

    await fake.close();
  });

  test("reserving pushes session.authorize to the owning provider", async () => {
    const fake = connectFake();
    await fake.connect();

    const target = devices[0]!.id;
    const res = await caller(USER_A).device.reserve({ deviceId: target });

    await Bun.sleep(100);
    const authorize = fake.received.find((c) => c.kind === "session.authorize");
    expect(authorize).toMatchObject({
      kind: "session.authorize",
      deviceId: target,
      reservationId: res.id,
      userId: USER_A,
    });

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("releasing pushes session.revoke", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices[0]!.id;

    await caller(USER_A).device.reserve({ deviceId: target });
    await caller(USER_A).device.release({ deviceId: target });
    await Bun.sleep(100);

    expect(fake.received.some((c) => c.kind === "session.revoke")).toBe(true);
    await fake.close();
  });

  test("a device on an offline provider cannot be reserved", async () => {
    // No fake connected: the row exists but nothing can serve it.
    await expect(caller(USER_A).device.reserve({ deviceId: devices[0]!.id })).rejects.toMatchObject(
      { code: "PRECONDITION_FAILED" },
    );
  });

  test("commands round-trip through the provider and return its data", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices.find((d) => d.platform === "android")!.id;

    await caller(USER_A).device.reserve({ deviceId: target });

    const apps = await caller(USER_A).device.apps({ deviceId: target });
    expect(apps).toHaveLength(2);
    expect(apps[0]).toMatchObject({ id: "com.example.app" });

    const adb = await caller(USER_A).device.adbExpose({ deviceId: target });
    expect(adb.port).toBeGreaterThan(0);
    expect(adb.connectString).toBe(`gw-test.example.com:${adb.port}`);

    const [stored] = await db.select().from(device).where(eq(device.id, target));
    expect(stored?.adbPort).toBe(adb.port);

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("a command from someone who does not hold the device is refused", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices[0]!.id;

    await caller(USER_A).device.reserve({ deviceId: target });
    await expect(caller(USER_B).device.reboot({ deviceId: target })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("a failing command surfaces the provider's error", async () => {
    const fake = connectFake({
      onCommand: (payload) => {
        if (payload.kind === "device.reboot") throw new Error("device is wedged");
        return undefined;
      },
    });
    await fake.connect();
    const target = devices[0]!.id;

    await caller(USER_A).device.reserve({ deviceId: target });
    await expect(caller(USER_A).device.reboot({ deviceId: target })).rejects.toThrow(
      "device is wedged",
    );

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("hotplug: upsert adds a device, removed marks it absent", async () => {
    const fake = connectFake();
    await fake.connect();

    const hotplugged = {
      ...devices[0]!,
      id: "gw-test-hotplug-1",
      name: "Hot-plugged phone",
    };
    fake.upsertDevice(hotplugged);
    await Bun.sleep(150);

    let [row] = await db.select().from(device).where(eq(device.id, hotplugged.id));
    expect(row?.name).toBe("Hot-plugged phone");
    expect(row?.status).toBe("ready");

    fake.removeDevice(hotplugged.id);
    await Bun.sleep(150);

    [row] = await db.select().from(device).where(eq(device.id, hotplugged.id));
    expect(row?.status).toBe("absent");

    await fake.close();
  });

  test("disconnect marks devices absent and releases their reservations", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices[0]!.id;

    await caller(USER_A).device.reserve({ deviceId: target });
    await fake.close();
    await Bun.sleep(200);

    const [p] = await db.select().from(provider).where(eq(provider.id, PROVIDER_ID));
    expect(p?.status).toBe("offline");

    const stored = await db.select().from(device).where(eq(device.providerId, PROVIDER_ID));
    expect(stored.every((d) => d.status === "absent")).toBe(true);

    const active = await db.select().from(reservation).where(eq(reservation.deviceId, target));
    expect(active.every((r) => r.state !== "active")).toBe(true);
  });

  test("session tokens are issued only to the reservation holder", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices[0]!.id;

    await expect(caller(USER_A).device.sessionToken({ deviceId: target })).rejects.toMatchObject({
      code: "PRECONDITION_FAILED",
    });

    await caller(USER_A).device.reserve({ deviceId: target });

    await expect(caller(USER_B).device.sessionToken({ deviceId: target })).rejects.toMatchObject({
      code: "FORBIDDEN",
    });

    const issued = await caller(USER_A).device.sessionToken({ deviceId: target });
    expect(issued.token.split(".")).toHaveLength(3);
    expect(issued.sessionUrl).toBe(`wss://gw-test.example.com/s/${target}`);
    expect(issued.expiresAt.getTime()).toBeGreaterThan(Date.now());

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("reported activity lands on the active reservation, and cannot run backwards", async () => {
    const fake = connectFake();
    await fake.connect();
    const target = devices[0]!.id;

    const held = await caller(USER_A).device.reserve({ deviceId: target });

    // Backdated first, so the report is unambiguously newer than the row.
    await db
      .update(reservation)
      .set({ lastActivityAt: new Date(Date.now() - 600_000) })
      .where(eq(reservation.id, held.id));

    const at = Date.now();
    fake.noteActivity(target, at);
    await Bun.sleep(200);

    const [updated] = await db.select().from(reservation).where(eq(reservation.id, held.id));
    expect(updated?.lastActivityAt.getTime()).toBe(at);

    // A provider whose clock is behind must not undo a later report.
    fake.noteActivity(target, at - 60_000);
    await Bun.sleep(200);

    const [unchanged] = await db.select().from(reservation).where(eq(reservation.id, held.id));
    expect(unchanged?.lastActivityAt.getTime()).toBe(at);

    await caller(USER_A).device.release({ deviceId: target });
    await fake.close();
  });

  test("the JWKS the provider will verify against is published and usable", async () => {
    const res = await fetch(`${baseUrl}/.well-known/farm-jwks.json`);
    expect(res.ok).toBe(true);

    const body = (await res.json()) as { keys: Array<Record<string, string>> };
    expect(body.keys).toHaveLength(1);

    const [key] = body.keys;
    expect(key?.kty).toBe("OKP");
    expect(key?.crv).toBe("Ed25519");
    expect(key?.alg).toBe("EdDSA");
    expect(key?.kid).toBeTruthy();
    // A private component here would hand every provider the signing key.
    expect(key?.d).toBeUndefined();
  });
});
