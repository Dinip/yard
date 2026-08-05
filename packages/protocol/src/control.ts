import { z } from "zod";
import { AppInfo, Battery, DeviceSnapshot, DeviceStatus, Display, Timestamp } from "./common.ts";
import { named } from "./registry.ts";

export const PROTOCOL_VERSION = 1;

/**
 * Control plane: provider ↔ coordinator, over the provider's single outbound
 * WSS to `/api/providers/connect`.
 *
 * JSON, not binary — these are low-rate and being able to read them in a log is
 * worth more than the bytes. Only video access units are binary framed, and
 * those live on the session plane.
 */

// ── commands: coordinator → provider ───────────────────────────────────────

export const CommandPayload = named(
  "CommandPayload",
  z.discriminatedUnion("kind", [
    /**
     * Tell the provider which reservation is currently allowed to open a
     * session for this device. The provider checks each session-plane JWT's
     * reservationId against this; the short token `exp` is only a backstop.
     */
    z.object({
      kind: z.literal("session.authorize"),
      deviceId: z.string(),
      reservationId: z.string(),
      userId: z.string(),
    }),
    /** Drop live viewers and refuse further connects for this device. */
    z.object({
      kind: z.literal("session.revoke"),
      deviceId: z.string(),
      reason: z.string().optional(),
    }),
    z.object({ kind: z.literal("device.reboot"), deviceId: z.string() }),
    z.object({ kind: z.literal("device.rotate"), deviceId: z.string(), degrees: z.number().int() }),
    z.object({ kind: z.literal("device.apps"), deviceId: z.string() }),
    z.object({
      kind: z.literal("device.launch"),
      deviceId: z.string(),
      appId: z.string(),
      args: z.array(z.string()).optional(),
    }),
    z.object({ kind: z.literal("device.uninstall"), deviceId: z.string(), appId: z.string() }),
    /** Android only. Binds a provider-host TCP port proxying an adb transport. */
    z.object({ kind: z.literal("device.adb.expose"), deviceId: z.string() }),
    z.object({ kind: z.literal("device.adb.unexpose"), deviceId: z.string() }),
    /** Tear down and re-establish the device's backend session. */
    z.object({ kind: z.literal("device.restart"), deviceId: z.string() }),
  ]),
);

export const CommandData = named(
  "CommandData",
  z.object({
    apps: z.array(AppInfo).optional(),
    adbPort: z.number().int().optional(),
  }),
);

// ── provider → coordinator ─────────────────────────────────────────────────

export const ProviderMessage = named(
  "ProviderMessage",
  z.discriminatedUnion("type", [
    /**
     * First frame after the socket opens. Carries the provider's whole device
     * inventory so the coordinator can reconcile in one shot: anything it has
     * for this provider that is absent here becomes `absent`.
     */
    z.object({
      type: z.literal("hello"),
      protocolVersion: z.number().int(),
      providerId: z.string(),
      name: z.string(),
      version: z.string(),
      /** The URL the *browser* will use. The coordinator never dials it. */
      publicBaseUrl: z.string(),
      hostname: z.string().optional(),
      devices: z.array(DeviceSnapshot),
    }),
    z.object({ type: z.literal("heartbeat"), at: Timestamp }),
    /** Full snapshot, not a delta — see DeviceSnapshot. */
    z.object({ type: z.literal("device.upsert"), device: DeviceSnapshot }),
    z.object({ type: z.literal("device.removed"), deviceId: z.string() }),
    z.object({
      type: z.literal("device.status"),
      deviceId: z.string(),
      status: DeviceStatus,
      note: z.string().optional(),
    }),
    z.object({ type: z.literal("device.display"), deviceId: z.string(), display: Display }),
    z.object({ type: z.literal("device.battery"), deviceId: z.string(), battery: Battery }),
    z.object({
      type: z.literal("command.result"),
      /** Correlates with CoordinatorMessage.command.id. */
      id: z.string(),
      ok: z.boolean(),
      error: z.string().optional(),
      data: CommandData.optional(),
    }),
    /** Recorded in the audit log; there is no artifact table to write to. */
    z.object({
      type: z.literal("install.finished"),
      deviceId: z.string(),
      userId: z.string(),
      filename: z.string(),
      size: z.number().int(),
      sha256: z.string(),
      ok: z.boolean(),
      error: z.string().optional(),
    }),
  ]),
);

// ── coordinator → provider ─────────────────────────────────────────────────

export const CoordinatorMessage = named(
  "CoordinatorMessage",
  z.discriminatedUnion("type", [
    z.object({
      type: z.literal("hello.ack"),
      protocolVersion: z.number().int(),
      heartbeatIntervalMs: z.number().int(),
      /** Absolute URL of the coordinator's JWKS; fetched once and cached. */
      jwksUrl: z.string(),
      /**
       * The `iss` every session token carries, which is the coordinator's
       * **public** URL.
       *
       * A provider cannot infer this from the address it dialled. Those are the
       * same string only in development: in any real deployment the provider
       * reaches the coordinator over an internal address or a service name,
       * while tokens are signed with the origin browsers use. Inferring it
       * meant every session was refused with `InvalidIssuer`.
       */
      issuer: z.string(),
      /**
       * Browser origins allowed to call the provider's session and artifact
       * planes. The provider is always a *different* origin from the web app —
       * that is the entire point of keeping the coordinator off the data path —
       * so uploads and screenshots are cross-origin requests and need CORS.
       *
       * It comes from the coordinator rather than provider config because the
       * coordinator is where policy lives, and because a provider that had to
       * be told separately would silently drift out of step with it.
       */
      webOrigins: z.array(z.string()),
    }),
    z.object({
      type: z.literal("hello.reject"),
      reason: z.string(),
    }),
    z.object({
      type: z.literal("command"),
      id: z.string(),
      payload: CommandPayload,
    }),
    z.object({ type: z.literal("ping"), at: Timestamp }),
  ]),
);

export type CommandPayload = z.infer<typeof CommandPayload>;
export type CommandData = z.infer<typeof CommandData>;
export type ProviderMessage = z.infer<typeof ProviderMessage>;
export type CoordinatorMessage = z.infer<typeof CoordinatorMessage>;
export type CommandKind = CommandPayload["kind"];
