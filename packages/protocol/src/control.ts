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

/**
 * Which parts of a between-users reset to run, straight from farm policy.
 *
 * Sent per command rather than configured on the provider because this is an
 * admin's decision about the farm, not an operator's about a host — the one
 * exception being the *paths* `wipeFolders` acts on, which stay in the
 * provider's YAML so that no web form ends in `rm -rf` on a phone.
 */
export const CleanupSteps = named(
  "CleanupSteps",
  z.object({
    /** Uninstall apps that appeared during the session, against a baseline. */
    uninstallApps: z.boolean(),
    /** Home, rotation back to 0, clipboard cleared. */
    resetScreen: z.boolean(),
    /** `pm clear` on surviving third-party apps. Android only. */
    clearAppData: z.boolean(),
    /** Empty the paths the provider was configured with. */
    wipeFolders: z.boolean(),
  }),
);

/**
 * Which app ids a step may touch, as `*` globs matched case-insensitively —
 * `com.google.*`, `*.google.*`, `com.acme.harness`.
 *
 * An empty `allow` means everything is in scope; a non-empty one narrows the
 * step to exactly what it lists. `deny` always wins. Clearing app data is
 * destructive to state an app may not survive losing — a signed-in MDM agent,
 * a test harness holding its own credentials — so the safe configuration is to
 * name what may be cleared rather than to guess at what may not.
 */
export const AppFilter = named(
  "AppFilter",
  z.object({
    allow: z.array(z.string()),
    deny: z.array(z.string()),
  }),
);

/**
 * A developer's ADB public key, as the coordinator knows it.
 *
 * `publicKey` is the base64 blob out of `~/.android/adbkey.pub`, carried
 * verbatim: it is what the provider verifies a challenge signature against, and
 * carrying a fingerprint alone would not be enough — ADB authentication is
 * challenge-response, so a signature cannot be checked without the key itself.
 */
export const AdbKey = named(
  "AdbKey",
  z.object({
    userId: z.string(),
    fingerprint: z.string(),
    publicKey: z.string(),
    /** The trailing comment in the key file, typically `user@host`. */
    comment: z.string().optional(),
  }),
);

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
      /**
       * Every ADB key entitled to this session — the holder's, plus those of
       * anyone the holder has approved into it. The provider admits an
       * `adb connect` locally against this set, so the common case never needs
       * the coordinator and survives a coordinator restart.
       */
      adbKeys: z.array(AdbKey),
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
    /**
     * Reset the device now that its reservation has ended. Sent after
     * `session.revoke`, and only when the provider is still connected.
     *
     * Fire-and-forget on purpose: a multi-package uninstall runs well past the
     * gateway's correlated-command timeout, so completion comes back as a
     * `cleanup.finished` event and a `device.status` push. The provider decides
     * which of these steps it can actually do — a step its backend does not
     * support is a no-op, not a failure. See docs/CLEANUP.md.
     */
    z.object({
      kind: z.literal("device.cleanup"),
      deviceId: z.string(),
      steps: CleanupSteps,
      /** Scopes `clearAppData`. Ignored by the other steps. */
      clearAppDataFilter: AppFilter,
      /** Whole-run deadline. The provider lands the device on `ready` regardless. */
      timeoutSeconds: z.number().int(),
    }),
    /** Android only. Binds a provider-host TCP port proxying an adb transport. */
    z.object({ kind: z.literal("device.adb.expose"), deviceId: z.string() }),
    z.object({ kind: z.literal("device.adb.unexpose"), deviceId: z.string() }),
    /**
     * Replace the set of keys allowed to `adb connect` this device.
     *
     * The whole set, never a delta — a key deleted in settings has to reach the
     * provider, and a dropped patch would leave it trusting a revoked key
     * forever. Same reason `hello` carries a whole inventory.
     */
    z.object({ kind: z.literal("device.adb.keys"), deviceId: z.string(), keys: z.array(AdbKey) }),
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
    /**
     * Someone drove this device. The provider is authoritative about it: it
     * sees input arriving on the session plane *and* installs, which is more
     * than the browser can vouch for, and it is the only thing that can see a
     * device being used through an exposed adb transport at all.
     *
     * Rate-limited by the provider — this exists to hold an idle timeout off,
     * not to be a log of every touch.
     */
    z.object({ type: z.literal("device.activity"), deviceId: z.string(), at: Timestamp }),
    z.object({
      type: z.literal("command.result"),
      /** Correlates with CoordinatorMessage.command.id. */
      id: z.string(),
      ok: z.boolean(),
      error: z.string().optional(),
      data: CommandData.optional(),
    }),
    /**
     * An `adb connect` arrived carrying a key the provider has never been told
     * about, and its connection is parked waiting for an answer.
     *
     * Provider-initiated, so it cannot ride `command.result`. The provider has
     * already proved the client holds the private half — it verified a
     * signature over a token it issued — so the only open question is whose key
     * this is, and that is the coordinator's to answer.
     */
    z.object({
      type: z.literal("adb.auth.request"),
      deviceId: z.string(),
      /** Correlates with the coordinator's `adb.auth.decision`. */
      requestId: z.string(),
      fingerprint: z.string(),
      publicKey: z.string(),
      comment: z.string().optional(),
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
    /**
     * A between-users reset finished, whatever the outcome.
     *
     * Audit-only, like `install.finished`: the device's own `ready` comes back
     * as a separate `device.status`, so losing this message costs a log entry
     * rather than stranding a device. `errors` is non-empty on a partial run —
     * steps do not abort each other — and a whole-run timeout arrives as one
     * error with the rest of the report filled in as far as it got.
     */
    z.object({
      type: z.literal("cleanup.finished"),
      deviceId: z.string(),
      /** Apps uninstalled, by id. */
      removed: z.array(z.string()),
      /** Apps whose data was cleared, by id. */
      cleared: z.array(z.string()),
      /** Paths emptied. */
      wiped: z.array(z.string()),
      /** One per failed step, already prefixed with the step's name. */
      errors: z.array(z.string()),
      durationMs: z.number().int(),
    }),
    /**
     * Bytes left the device. The coordinator is not on that path — the download
     * went browser↔provider like every other artifact — so this is the only way
     * it can record what was taken, which it must: this is the one operation in
     * the system that carries data *out* of a device.
     *
     * Sent on success only, and only for a file. Listing a directory is a
     * metadata read and would write a row per click.
     */
    z.object({
      type: z.literal("file.pulled"),
      deviceId: z.string(),
      userId: z.string(),
      path: z.string(),
      size: z.number().int(),
      sha256: z.string(),
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
    /**
     * The answer to an `adb.auth.request`: admit the parked connection or close
     * it.
     *
     * An approval is also written to the key's owner and followed by a
     * `device.adb.keys` refresh, but *this* is what admits *this* connection.
     * Waiting for the refreshed set instead would make admission depend on the
     * order two messages happen to arrive in.
     */
    z.object({
      type: z.literal("adb.auth.decision"),
      requestId: z.string(),
      allow: z.boolean(),
      /** Who the key belongs to. Present when `allow`. */
      userId: z.string().optional(),
      /** Shown to the developer on the refusal. */
      reason: z.string().optional(),
    }),
    z.object({ type: z.literal("ping"), at: Timestamp }),
  ]),
);

export type AdbKey = z.infer<typeof AdbKey>;
export type CommandPayload = z.infer<typeof CommandPayload>;
export type CommandData = z.infer<typeof CommandData>;
export type ProviderMessage = z.infer<typeof ProviderMessage>;
export type CoordinatorMessage = z.infer<typeof CoordinatorMessage>;
export type CommandKind = CommandPayload["kind"];
