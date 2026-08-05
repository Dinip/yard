import { device, provider, providerToken, reservation } from "@farm/db";
import {
  type CoordinatorMessage,
  type DeviceSnapshot,
  JWKS_PATH,
  PROTOCOL_VERSION,
  ProviderMessage,
} from "@farm/protocol";
import { and, eq, inArray, notInArray } from "drizzle-orm";
import { db } from "../db.ts";
import { env } from "../env.ts";
import { audit } from "../lib/audit.ts";
import { deviceEvents } from "../lib/events.ts";
import { hashProviderToken } from "../lib/provider-token.ts";
import { ProviderConnection, providers } from "./registry.ts";

export const HEARTBEAT_INTERVAL_MS = 15_000;
/** Three missed heartbeats. Generous: a GC pause must not drop a provider. */
export const HEARTBEAT_TIMEOUT_MS = HEARTBEAT_INTERVAL_MS * 3 + 5_000;

export interface AuthedProvider {
  providerId: string;
  tokenId: string;
}

/**
 * Validates a provider's bearer token before the socket is upgraded.
 * Returns null when the credential is unusable, so the caller can 401 rather
 * than accept a socket it will immediately close.
 */
export async function authenticateProvider(token: string): Promise<AuthedProvider | null> {
  const hash = hashProviderToken(token);
  const [row] = await db
    .select({
      id: providerToken.id,
      providerId: providerToken.providerId,
      revokedAt: providerToken.revokedAt,
    })
    .from(providerToken)
    .where(eq(providerToken.tokenHash, hash))
    .limit(1);

  if (!row || row.revokedAt) return null;

  await db
    .update(providerToken)
    .set({ lastUsedAt: new Date() })
    .where(eq(providerToken.id, row.id));

  return { providerId: row.providerId, tokenId: row.id };
}

/**
 * The control-plane state machine for one provider socket.
 *
 * hello → reconcile inventory → heartbeat loop → event ingest → command results
 *
 * Instantiated by whichever transport accepted the socket; `send` and `close`
 * are injected so the whole machine is testable without a real WebSocket.
 */
export class GatewaySession {
  private conn: ProviderConnection | null = null;
  private helloSeen = false;
  private heartbeatTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly auth: AuthedProvider,
    private readonly send: (msg: CoordinatorMessage) => void,
    private readonly closeSocket: (code: number, reason: string) => void,
  ) {}

  async onMessage(raw: string) {
    let parsed: ProviderMessage;
    try {
      parsed = ProviderMessage.parse(JSON.parse(raw));
    } catch (err) {
      // A message we cannot parse means the two sides disagree about the wire
      // contract. Failing loudly beats silently ignoring device state.
      console.warn(`[gateway] ${this.auth.providerId}: unparseable message:`, err);
      this.reject("malformed message");
      return;
    }

    if (!this.helloSeen && parsed.type !== "hello") {
      this.reject("expected hello as the first message");
      return;
    }

    try {
      await this.dispatch(parsed);
    } catch (err) {
      console.error(`[gateway] ${this.auth.providerId}: handling ${parsed.type} failed:`, err);
    }
  }

  private async dispatch(msg: ProviderMessage) {
    switch (msg.type) {
      case "hello":
        await this.onHello(msg);
        break;

      case "heartbeat":
        this.armHeartbeat();
        await db
          .update(provider)
          .set({ lastSeenAt: new Date() })
          .where(eq(provider.id, this.auth.providerId));
        break;

      case "device.upsert":
        await this.upsertDevice(msg.device);
        deviceEvents.publish();
        break;

      case "device.removed":
        await this.markAbsent([msg.deviceId]);
        deviceEvents.publish();
        break;

      case "device.status":
        await db
          .update(device)
          .set({ status: msg.status, notes: msg.note ?? null, updatedAt: new Date() })
          .where(and(eq(device.id, msg.deviceId), eq(device.providerId, this.auth.providerId)));
        deviceEvents.publish();
        break;

      case "device.display":
        await db
          .update(device)
          .set({
            displayWidth: msg.display.width,
            displayHeight: msg.display.height,
            displayScale: msg.display.scale ?? null,
            displayRotation: msg.display.rotation ?? null,
            updatedAt: new Date(),
          })
          .where(and(eq(device.id, msg.deviceId), eq(device.providerId, this.auth.providerId)));
        deviceEvents.publish();
        break;

      case "device.battery":
        await db
          .update(device)
          .set({
            batteryLevel: msg.battery.level ?? null,
            batteryState: msg.battery.state ?? null,
            updatedAt: new Date(),
          })
          .where(and(eq(device.id, msg.deviceId), eq(device.providerId, this.auth.providerId)));
        // Battery alone does not warrant waking every subscriber.
        break;

      case "command.result":
        this.conn?.settle(msg.id, msg.ok, msg.data, msg.error);
        break;

      case "install.finished":
        // The uploaded file is already deleted by the time this arrives — this
        // row is the only record that the install ever happened.
        await audit(db, msg.userId, "device.install", "device", msg.deviceId, {
          filename: msg.filename,
          size: msg.size,
          sha256: msg.sha256,
          ok: msg.ok,
          error: msg.error,
          providerId: this.auth.providerId,
        });
        break;
    }
  }

  private async onHello(msg: Extract<ProviderMessage, { type: "hello" }>) {
    if (msg.protocolVersion !== PROTOCOL_VERSION) {
      this.reject(
        `protocol version mismatch: coordinator speaks ${PROTOCOL_VERSION}, provider speaks ${msg.protocolVersion}`,
      );
      return;
    }
    // The token identifies the provider; a mismatched self-reported id means a
    // token was copied to the wrong host, which we must not paper over.
    if (msg.providerId !== this.auth.providerId) {
      this.reject(
        `token belongs to provider ${this.auth.providerId}, but hello claims ${msg.providerId}`,
      );
      return;
    }

    this.helloSeen = true;

    await db
      .update(provider)
      .set({
        name: msg.name,
        version: msg.version,
        publicBaseUrl: msg.publicBaseUrl,
        hostname: msg.hostname ?? null,
        status: "online",
        lastSeenAt: new Date(),
        updatedAt: new Date(),
      })
      .where(eq(provider.id, this.auth.providerId));

    await this.reconcile(msg.devices);

    this.conn = new ProviderConnection(this.auth.providerId, msg.publicBaseUrl, this.send);
    providers.add(this.conn);

    this.send({
      type: "hello.ack",
      protocolVersion: PROTOCOL_VERSION,
      heartbeatIntervalMs: HEARTBEAT_INTERVAL_MS,
      jwksUrl: new URL(JWKS_PATH, env.PUBLIC_URL).toString(),
      webOrigins: env.WEB_ORIGIN,
    });

    this.armHeartbeat();
    deviceEvents.publish();

    console.log(
      `[gateway] ${msg.name} (${this.auth.providerId}) online — ${msg.devices.length} device(s) at ${msg.publicBaseUrl}`,
    );
  }

  /**
   * Brings the database in line with the provider's whole inventory in one
   * shot: everything present is upserted, and anything we still have for this
   * provider that the provider no longer reports becomes `absent`.
   *
   * This is why `hello` carries the full device list rather than a diff — a
   * provider that crashed mid-change cannot leave a stale row behind.
   */
  private async reconcile(snapshots: DeviceSnapshot[]) {
    for (const snapshot of snapshots) {
      await this.upsertDevice(snapshot);
    }

    const ids = snapshots.map((d) => d.id);
    const stale = ids.length
      ? and(eq(device.providerId, this.auth.providerId), notInArray(device.id, ids))
      : eq(device.providerId, this.auth.providerId);

    await db.update(device).set({ status: "absent", updatedAt: new Date() }).where(stale);
  }

  private async upsertDevice(snapshot: DeviceSnapshot) {
    const values = {
      id: snapshot.id,
      providerId: this.auth.providerId,
      platform: snapshot.platform,
      status: snapshot.status,
      name: snapshot.name ?? null,
      model: snapshot.model ?? null,
      manufacturer: snapshot.manufacturer ?? null,
      osVersion: snapshot.osVersion ?? null,
      abi: snapshot.abi ?? null,
      sdk: snapshot.sdk ?? null,
      displayWidth: snapshot.display?.width ?? null,
      displayHeight: snapshot.display?.height ?? null,
      displayScale: snapshot.display?.scale ?? null,
      displayRotation: snapshot.display?.rotation ?? null,
      batteryLevel: snapshot.battery?.level ?? null,
      batteryState: snapshot.battery?.state ?? null,
      streamCodec: snapshot.streamCodec ?? null,
      adbPort: snapshot.adbPort ?? null,
      notes: snapshot.note ?? null,
      updatedAt: new Date(),
      presentAt: snapshot.status === "absent" ? null : new Date(),
      readyAt: snapshot.status === "ready" ? new Date() : null,
    };

    await db
      .insert(device)
      .values(values)
      .onConflictDoUpdate({
        target: device.id,
        set: { ...values, id: undefined },
      });
  }

  private async markAbsent(deviceIds: string[]) {
    if (deviceIds.length === 0) return;
    await db
      .update(device)
      .set({ status: "absent", updatedAt: new Date() })
      .where(and(eq(device.providerId, this.auth.providerId), inArray(device.id, deviceIds)));
  }

  /**
   * A provider that stops heartbeating is treated exactly like one that
   * disconnected. Without this a half-open TCP connection would keep its
   * devices looking `ready` indefinitely.
   */
  private armHeartbeat() {
    if (this.heartbeatTimer) clearTimeout(this.heartbeatTimer);
    this.heartbeatTimer = setTimeout(() => {
      console.warn(`[gateway] ${this.auth.providerId}: heartbeat timeout — closing`);
      this.closeSocket(4008, "heartbeat timeout");
    }, HEARTBEAT_TIMEOUT_MS);
  }

  private reject(reason: string) {
    console.warn(`[gateway] ${this.auth.providerId}: rejected — ${reason}`);
    this.send({ type: "hello.reject", reason });
    this.closeSocket(4001, reason);
  }

  /**
   * Socket is gone. Everything this provider owned becomes unusable, and its
   * reservations are released — a device nobody can reach must not stay locked
   * to the user who happened to hold it.
   */
  async onClose() {
    if (this.heartbeatTimer) clearTimeout(this.heartbeatTimer);
    if (this.conn) providers.remove(this.conn);
    this.conn = null;

    await db
      .update(provider)
      .set({ status: "offline", updatedAt: new Date() })
      .where(eq(provider.id, this.auth.providerId));

    const owned = await db
      .select({ id: device.id })
      .from(device)
      .where(eq(device.providerId, this.auth.providerId));
    const ids = owned.map((d) => d.id);

    if (ids.length) {
      await db
        .update(device)
        .set({ status: "absent", updatedAt: new Date() })
        .where(eq(device.providerId, this.auth.providerId));

      await db
        .update(reservation)
        .set({
          state: "released",
          releasedAt: new Date(),
          reason: "provider disconnected",
        })
        .where(and(inArray(reservation.deviceId, ids), eq(reservation.state, "active")));
    }

    deviceEvents.publish();
    console.log(`[gateway] ${this.auth.providerId} offline — ${ids.length} device(s) absent`);
  }
}
