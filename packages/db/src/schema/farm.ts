import { sql } from "drizzle-orm";
import {
  index,
  integer,
  jsonb,
  pgEnum,
  pgTable,
  real,
  text,
  timestamp,
  uniqueIndex,
} from "drizzle-orm/pg-core";
import { user } from "./auth.ts";

export const platformEnum = pgEnum("platform", ["ios", "android"]);

export const providerStatusEnum = pgEnum("provider_status", ["online", "offline"]);

/**
 * Device lifecycle:
 *   absent    — the owning provider is gone, or the device unplugged
 *   present   — seen by the provider, not yet usable
 *   preparing — tunnel/stream coming up
 *   ready     — reservable
 *   busy      — an active reservation exists
 *   unhealthy — present but the backend reported a fault
 */
export const deviceStatusEnum = pgEnum("device_status", [
  "absent",
  "present",
  "preparing",
  "ready",
  "busy",
  "unhealthy",
]);

export const reservationStateEnum = pgEnum("reservation_state", ["active", "released", "expired"]);

export const provider = pgTable("provider", {
  id: text("id").primaryKey(),
  name: text("name").notNull(),
  /** Base URL the *browser* uses to reach this provider directly (session + artifact planes). */
  publicBaseUrl: text("public_base_url").notNull(),
  hostname: text("hostname"),
  version: text("version"),
  status: providerStatusEnum("status").notNull().default("offline"),
  lastSeenAt: timestamp("last_seen_at"),
  createdAt: timestamp("created_at").notNull().defaultNow(),
  updatedAt: timestamp("updated_at").notNull().defaultNow(),
});

/**
 * Machine credentials for providers. Deliberately outside better-auth: these are
 * not users, never carry a session, and are rotated by admins.
 */
export const providerToken = pgTable(
  "provider_token",
  {
    id: text("id").primaryKey(),
    providerId: text("provider_id")
      .notNull()
      .references(() => provider.id, { onDelete: "cascade" }),
    /** sha256 of the presented secret; the plaintext is shown once at creation. */
    tokenHash: text("token_hash").notNull().unique(),
    label: text("label"),
    createdBy: text("created_by").references(() => user.id, { onDelete: "set null" }),
    createdAt: timestamp("created_at").notNull().defaultNow(),
    lastUsedAt: timestamp("last_used_at"),
    revokedAt: timestamp("revoked_at"),
  },
  (t) => [index("provider_token_provider_idx").on(t.providerId)],
);

export const device = pgTable(
  "device",
  {
    /** udid (iOS) or serial (Android). Stable across reconnects. */
    id: text("id").primaryKey(),
    providerId: text("provider_id")
      .notNull()
      .references(() => provider.id, { onDelete: "cascade" }),
    platform: platformEnum("platform").notNull(),
    name: text("name"),
    model: text("model"),
    manufacturer: text("manufacturer"),
    osVersion: text("os_version"),
    /** Android: primary ABI. iOS: cpu architecture. */
    abi: text("abi"),
    /** Android SDK level. */
    sdk: integer("sdk"),
    /**
     * Identity for filing a bug against the right device. Android-only in
     * practice: all of it comes from the provider's existing `getprop` read.
     */
    serial: text("serial"),
    brand: text("brand"),
    buildId: text("build_id"),
    securityPatch: text("security_patch"),
    abiList: text("abi_list"),
    displayWidth: integer("display_width"),
    displayHeight: integer("display_height"),
    displayScale: real("display_scale"),
    displayRotation: integer("display_rotation"),
    batteryLevel: real("battery_level"),
    batteryState: text("battery_state"),
    status: deviceStatusEnum("status").notNull().default("absent"),
    /** e.g. "hev1.1.6.L93.B0" or "avc1.640028" — handed to the browser's VideoDecoder. */
    streamCodec: text("stream_codec"),
    /** Port on the provider host exposing an adb transport, when requested. */
    adbPort: integer("adb_port"),
    presentAt: timestamp("present_at"),
    readyAt: timestamp("ready_at"),
    notes: text("notes"),
    createdAt: timestamp("created_at").notNull().defaultNow(),
    updatedAt: timestamp("updated_at").notNull().defaultNow(),
  },
  (t) => [index("device_provider_idx").on(t.providerId), index("device_status_idx").on(t.status)],
);

/**
 * Replaces STF's device-owned "group" model. The database is the arbiter of
 * ownership — the partial unique index below makes two concurrent reserve calls
 * impossible to both win, without any application-level locking.
 */
export const reservation = pgTable(
  "reservation",
  {
    id: text("id").primaryKey(),
    deviceId: text("device_id")
      .notNull()
      .references(() => device.id, { onDelete: "cascade" }),
    userId: text("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    state: reservationStateEnum("state").notNull().default("active"),
    startedAt: timestamp("started_at").notNull().defaultNow(),
    expiresAt: timestamp("expires_at").notNull(),
    /**
     * Last time anyone actually drove this device, from either of two sources:
     * the provider, which sees input and installs (including through an exposed
     * adb transport), and the browser, which can vouch for a tab being used
     * while no frames flow. Neither may move it backwards — see the idle reaper.
     */
    lastActivityAt: timestamp("last_activity_at").notNull().defaultNow(),
    releasedAt: timestamp("released_at"),
    releasedBy: text("released_by").references(() => user.id, { onDelete: "set null" }),
    reason: text("reason"),
  },
  (t) => [
    uniqueIndex("reservation_one_active_per_device")
      .on(t.deviceId)
      .where(sql`${t.state} = 'active'`),
    index("reservation_user_idx").on(t.userId),
    index("reservation_expires_idx").on(t.expiresAt).where(sql`${t.state} = 'active'`),
    // The idle sweep runs beside the expiry one, on the same cadence, and wants
    // the same shape of index.
    index("reservation_activity_idx").on(t.lastActivityAt).where(sql`${t.state} = 'active'`),
  ],
);

/**
 * An admin watching (and driving) somebody else's session.
 *
 * A table rather than a column on `reservation` because join and leave are
 * events worth querying — "who was in this session, and when" is an audit
 * question — and because the holder's UI names the people who are present.
 *
 * The provider needs no notion of this at all: it matches sessions on
 * `reservationId`, so an admin holding a token minted against the holder's
 * reservation is accepted exactly like the holder's second tab.
 */
export const reservationObserver = pgTable(
  "reservation_observer",
  {
    id: text("id").primaryKey(),
    reservationId: text("reservation_id")
      .notNull()
      .references(() => reservation.id, { onDelete: "cascade" }),
    userId: text("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    joinedAt: timestamp("joined_at").notNull().defaultNow(),
    leftAt: timestamp("left_at"),
  },
  (t) => [
    index("reservation_observer_reservation_idx").on(t.reservationId),
    // "Who is in this session right now" is the read the holder's UI makes on
    // every device fetch.
    uniqueIndex("reservation_observer_one_open_per_user")
      .on(t.reservationId, t.userId)
      .where(sql`${t.leftAt} is null`),
  ],
);

/**
 * Global configuration an admin can change without a redeploy.
 *
 * Key/value rather than a column per knob: these are policy, they arrive one at
 * a time, and a migration per setting would make adding one a deploy. Defaults
 * live in the coordinator's typed registry, not here — an absent row means "use
 * the default", so seeding is never required and a value can be reset by
 * deleting it.
 */
export const setting = pgTable("setting", {
  key: text("key").primaryKey(),
  /**
   * Nullable because `null` is a meaningful value for several keys — an idle
   * timeout of `null` is "off", which is not the same as never having been
   * configured. Absence of the *row* is what means unset.
   */
  value: jsonb("value"),
  updatedAt: timestamp("updated_at").notNull().defaultNow(),
  updatedBy: text("updated_by").references(() => user.id, { onDelete: "set null" }),
});

/**
 * There is no artifact/app table by design — uploads are transient and deleted
 * after install. An install therefore leaves its only trace here.
 */
export const auditLog = pgTable(
  "audit_log",
  {
    id: text("id").primaryKey(),
    actorUserId: text("actor_user_id").references(() => user.id, { onDelete: "set null" }),
    action: text("action").notNull(),
    targetType: text("target_type"),
    targetId: text("target_id"),
    metadata: jsonb("metadata").$type<Record<string, unknown>>(),
    at: timestamp("at").notNull().defaultNow(),
  },
  (t) => [
    index("audit_log_at_idx").on(t.at),
    index("audit_log_actor_idx").on(t.actorUserId),
    // "Everything that ever happened to this device" is the question an audit
    // log is most often opened to answer.
    index("audit_log_target_idx").on(t.targetId),
  ],
);

export type Provider = typeof provider.$inferSelect;
export type ProviderToken = typeof providerToken.$inferSelect;
export type Device = typeof device.$inferSelect;
export type Reservation = typeof reservation.$inferSelect;
export type AuditLog = typeof auditLog.$inferSelect;
export type Setting = typeof setting.$inferSelect;
export type ReservationObserver = typeof reservationObserver.$inferSelect;
export type Platform = (typeof platformEnum.enumValues)[number];
export type DeviceStatus = (typeof deviceStatusEnum.enumValues)[number];
