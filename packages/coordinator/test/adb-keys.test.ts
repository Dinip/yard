/**
 * Key management and the approval path, against a real Postgres and a real
 * provider socket.
 *
 * The claim under test is that a provider only ever holds keys the coordinator
 * currently considers entitled — the push happens on reserve, on add, on
 * remove, and when the observer set changes — and that a parked connection is
 * admitted by the decision rather than by the key push that follows it.
 */
import { afterAll, afterEach, beforeAll, describe, expect, test } from "bun:test";
import { device, provider, providerToken, reservation, user, userAdbKey } from "@yard/db";
import { parseAdbPublicKey } from "@yard/protocol/adbkey";
import { FakeProvider } from "@yard/protocol/test/fake-provider";
import { eq, inArray } from "drizzle-orm";
import { app } from "../src/app.ts";
import { db } from "../src/db.ts";
import { gatewayWebSocket } from "../src/gateway/route.ts";
import { generateProviderToken } from "../src/lib/provider-token.ts";
import { caller as callerFor, closePoolOnExit, testUser } from "./helpers.ts";

closePoolOnExit();

const PROVIDER_ID = "adbkeys-provider";
const DEVICE_ID = "adbkeys-device-0001";
const HOLDER = "adbkeys-holder";
const OTHER = "adbkeys-other";
const USERS = [HOLDER, OTHER];

const KEY = await Bun.file(
  new URL("../../protocol/test/vectors/adbkey.pub", import.meta.url),
).text();
const OTHER_KEY = await Bun.file(
  new URL("../../protocol/test/vectors/adbkey-other.pub", import.meta.url),
).text();

let server: ReturnType<typeof Bun.serve>;
let fake: FakeProvider;

beforeAll(async () => {
  await cleanup();
  server = Bun.serve({ port: 0, fetch: app.fetch, websocket: gatewayWebSocket });
  await db.insert(user).values(USERS.map(testUser));
  await db.insert(provider).values({
    id: PROVIDER_ID,
    name: "adb keys",
    publicBaseUrl: "https://provider.test",
    status: "offline",
  });
  const generated = generateProviderToken();
  await db.insert(providerToken).values({
    id: "adbkeys-token",
    providerId: PROVIDER_ID,
    tokenHash: generated.hash,
    label: "test",
  });

  fake = new FakeProvider({
    url: `http://localhost:${server.port}`,
    token: generated.plaintext,
    providerId: PROVIDER_ID,
    devices: [
      {
        id: DEVICE_ID,
        platform: "android",
        name: "Test Pixel",
        status: "ready",
        healthy: true,
      },
    ],
  });
  await fake.connect();
});

afterAll(async () => {
  await fake?.close();
  server?.stop(true);
  await cleanup();
});

afterEach(async () => {
  await db.delete(userAdbKey).where(inArray(userAdbKey.userId, USERS));
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db.update(device).set({ status: "ready" }).where(eq(device.id, DEVICE_ID));
  fake.received.length = 0;
  fake.adbDecisions.length = 0;
});

async function cleanup() {
  await db.delete(reservation).where(eq(reservation.deviceId, DEVICE_ID));
  await db.delete(device).where(eq(device.id, DEVICE_ID));
  await db.delete(providerToken).where(eq(providerToken.providerId, PROVIDER_ID));
  await db.delete(provider).where(eq(provider.id, PROVIDER_ID));
  await db.delete(userAdbKey).where(inArray(userAdbKey.userId, USERS));
  await db.delete(user).where(inArray(user.id, USERS));
}

/** The control plane is a socket; a push is not observable synchronously. */
async function settle() {
  await new Promise((resolve) => setTimeout(resolve, 150));
}

function pushedKeys() {
  return fake.received.filter((c) => c.kind === "device.adb.keys");
}

describe("registering a key", () => {
  test("parses the file, derives the fingerprint, and keeps the comment", async () => {
    const created = await callerFor(HOLDER).user.adbKeys.add({
      publicKey: KEY,
      title: "Laptop",
    });

    expect(created.fingerprint).toBe(parseAdbPublicKey(KEY).fingerprint);
    expect(created.comment).toBe("dev@example.test");
    expect(created.title).toBe("Laptop");
  });

  test("the same key twice is a conflict, on one account or two", async () => {
    await callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Laptop" });

    await expect(
      callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Again" }),
    ).rejects.toThrow(/already registered/);

    // The index is global on purpose: a key identifies one person, or "who ran
    // this adb shell" has no answer.
    await expect(
      callerFor(OTHER).user.adbKeys.add({ publicKey: KEY, title: "Mine now" }),
    ).rejects.toThrow(/already registered/);
  });

  test("a private key, or anything else, is refused with a usable message", async () => {
    await expect(
      callerFor(HOLDER).user.adbKeys.add({ publicKey: "not-a-key", title: "x" }),
    ).rejects.toThrow();
  });

  test("only your own keys are listed, and only your own can be removed", async () => {
    const mine = await callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Mine" });
    await callerFor(OTHER).user.adbKeys.add({ publicKey: OTHER_KEY, title: "Theirs" });

    const listed = await callerFor(HOLDER).user.adbKeys.list();
    expect(listed.map((k) => k.title)).toEqual(["Mine"]);

    await expect(callerFor(OTHER).user.adbKeys.remove({ id: mine.id })).rejects.toThrow();
    expect(await callerFor(HOLDER).user.adbKeys.remove({ id: mine.id })).toEqual({ ok: true });
  });
});

describe("pushing the entitled set", () => {
  test("reserving carries the holder's keys, so the first connect is silent", async () => {
    await callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Laptop" });
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();

    const authorize = fake.received.find((c) => c.kind === "session.authorize");
    expect(authorize).toBeDefined();
    expect(authorize?.kind === "session.authorize" && authorize.adbKeys).toHaveLength(1);
    expect(authorize?.kind === "session.authorize" && authorize.adbKeys[0]?.userId).toBe(HOLDER);
  });

  test("adding a key mid-session pushes it without waiting for the next reserve", async () => {
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();
    fake.received.length = 0;

    await callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Laptop" });
    await settle();

    const pushed = pushedKeys();
    expect(pushed).toHaveLength(1);
    expect(pushed[0]?.kind === "device.adb.keys" && pushed[0].keys).toHaveLength(1);
  });

  test("removing a key pushes the set without it, rather than a delta", async () => {
    const key = await callerFor(HOLDER).user.adbKeys.add({ publicKey: KEY, title: "Laptop" });
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();
    fake.received.length = 0;

    await callerFor(HOLDER).user.adbKeys.remove({ id: key.id });
    await settle();

    const pushed = pushedKeys();
    expect(pushed).toHaveLength(1);
    expect(pushed[0]?.kind === "device.adb.keys" && pushed[0].keys).toEqual([]);
  });

  test("a key belonging to nobody in the session is not pushed anywhere", async () => {
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();
    fake.received.length = 0;

    // OTHER holds nothing and is in no session, so this reaches no provider.
    await callerFor(OTHER).user.adbKeys.add({ publicKey: OTHER_KEY, title: "Theirs" });
    await settle();

    expect(pushedKeys()).toHaveLength(0);
  });

  test("an observer's keys arrive when they are let in, and leave when they go", async () => {
    await callerFor(OTHER).user.adbKeys.add({ publicKey: OTHER_KEY, title: "Theirs" });
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();
    fake.received.length = 0;

    const request = await callerFor(OTHER).device.requestJoin({ deviceId: DEVICE_ID });
    await callerFor(HOLDER).device.answerJoinRequest({ requestId: request.id, approve: true });
    await settle();

    const afterJoin = pushedKeys().at(-1);
    expect(afterJoin?.kind === "device.adb.keys" && afterJoin.keys.map((k) => k.userId)).toEqual([
      OTHER,
    ]);

    fake.received.length = 0;
    await callerFor(OTHER).device.leaveSession({ deviceId: DEVICE_ID });
    await settle();

    const afterLeave = pushedKeys().at(-1);
    expect(afterLeave?.kind === "device.adb.keys" && afterLeave.keys).toEqual([]);
  });
});

describe("an unknown key at the door", () => {
  const parsed = parseAdbPublicKey(KEY);

  async function ask() {
    await callerFor(HOLDER).device.reserve({ deviceId: DEVICE_ID });
    await settle();
    fake.received.length = 0;
    return fake.askAboutAdbKey({
      deviceId: DEVICE_ID,
      fingerprint: parsed.fingerprint,
      publicKey: parsed.publicKey,
      comment: parsed.comment,
    });
  }

  test("the holder sees it on the device, with a deadline", async () => {
    const { requestId } = await ask();
    await settle();

    const found = await callerFor(HOLDER).device.get({ id: DEVICE_ID });
    const [pending] = found.reservation?.adbAuthRequests ?? [];
    expect(pending?.requestId).toBe(requestId);
    expect(pending?.fingerprint).toBe(parsed.fingerprint);
    expect(pending?.expiresAt.getTime()).toBeGreaterThan(Date.now());
  });

  test("approving admits the connection and registers the key on the holder", async () => {
    const { requestId, answered } = await ask();
    await settle();

    await callerFor(HOLDER).device.answerAdbAuthRequest({ requestId, approve: true });

    const decision = await answered;
    expect(decision.allow).toBe(true);
    expect(decision.userId).toBe(HOLDER);

    const [stored] = await db.select().from(userAdbKey).where(eq(userAdbKey.userId, HOLDER));
    expect(stored?.fingerprint).toBe(parsed.fingerprint);

    // The refresh follows the decision; it is not what admitted the connection.
    await settle();
    expect(pushedKeys().length).toBeGreaterThan(0);
  });

  test("denying refuses it and stores nothing", async () => {
    const { requestId, answered } = await ask();
    await settle();

    await callerFor(HOLDER).device.answerAdbAuthRequest({ requestId, approve: false });

    expect((await answered).allow).toBe(false);
    expect(await db.select().from(userAdbKey).where(eq(userAdbKey.userId, HOLDER))).toHaveLength(0);
  });

  test("somebody else's session is not theirs to answer", async () => {
    const { requestId } = await ask();
    await settle();

    await expect(
      callerFor(OTHER).device.answerAdbAuthRequest({ requestId, approve: true }),
    ).rejects.toThrow(/not your session/);
  });

  test("a request can only be answered once", async () => {
    const { requestId } = await ask();
    await settle();

    await callerFor(HOLDER).device.answerAdbAuthRequest({ requestId, approve: true });
    await expect(
      callerFor(HOLDER).device.answerAdbAuthRequest({ requestId, approve: true }),
    ).rejects.toThrow(/no longer open/);
  });

  test("ending the session takes the question with it", async () => {
    const { requestId } = await ask();
    await settle();

    await callerFor(HOLDER).device.release({ deviceId: DEVICE_ID });
    await settle();

    await expect(
      callerFor(HOLDER).device.answerAdbAuthRequest({ requestId, approve: true }),
    ).rejects.toThrow(/no longer open/);
  });
});
