import type { Database } from "@farm/db";
import { setting } from "@farm/db";
import { eq } from "drizzle-orm";
import { z } from "zod";
import { env } from "../env.ts";

/**
 * Admin-editable global policy, the first DB-backed configuration in the
 * project.
 *
 * Three rules make it safe to read from anywhere:
 *
 * - **Every key is declared here** with a zod schema and a default, so a bad
 *   row (hand-edited, or left over from an older shape) falls back rather than
 *   propagating a wrong type into policy code.
 * - **Defaults come from the env vars these replace**, so behaviour is
 *   unchanged until an admin actually sets something, and a fresh database
 *   needs no seeding.
 * - **Reads are cached for a few seconds.** These are consulted on every
 *   reserve, renew and reaper sweep; a query each time would put the settings
 *   table on the hot path of the busiest thing the coordinator does.
 */
const SECONDS = z
  .number()
  .int()
  .min(30)
  .max(30 * 24 * 3600);

/** `null` is a real value here: it means the policy is off, not unset. */
const OPTIONAL_SECONDS = SECONDS.nullable();

export const SETTINGS = {
  "reservation.ttlSeconds": {
    schema: SECONDS,
    default: () => env.RESERVATION_TTL,
  },
  /** Release a reservation nobody has interacted with for this long. */
  "reservation.idleTimeoutSeconds": {
    schema: OPTIONAL_SECONDS,
    default: () => null,
  },
  /** Hard cap on a single reservation, however active it is. */
  "reservation.maxDurationSeconds": {
    schema: OPTIONAL_SECONDS,
    default: () => null,
  },
} as const satisfies Record<string, { schema: z.ZodTypeAny; default: () => unknown }>;

export type SettingKey = keyof typeof SETTINGS;
export type SettingValue<K extends SettingKey> = z.infer<(typeof SETTINGS)[K]["schema"]>;
export type Settings = { [K in SettingKey]: SettingValue<K> };

export const SETTING_KEYS = Object.keys(SETTINGS) as SettingKey[];

/**
 * Validates one key/value pair against that key's own schema.
 *
 * The value cannot be typed by the input schema alone — it depends on the key —
 * so the router accepts `unknown` and narrows here, which keeps the declaration
 * of what a setting *is* in exactly one place.
 */
export function parseSetting(key: SettingKey, value: unknown) {
  return SETTINGS[key].schema.safeParse(value);
}

/** Short enough that an admin's change lands "immediately" to a human. */
const CACHE_TTL_MS = 5_000;

let cache: { at: number; values: Settings } | null = null;

export function defaults(): Settings {
  const out = {} as Record<string, unknown>;
  for (const key of SETTING_KEYS) out[key] = SETTINGS[key].default();
  return out as Settings;
}

/** Drop the cache — called after a write so the admin sees their own change. */
export function invalidateSettings() {
  cache = null;
}

export async function getSettings(db: Database): Promise<Settings> {
  if (cache && Date.now() - cache.at < CACHE_TTL_MS) return cache.values;

  const values = defaults();
  try {
    const rows = await db.select().from(setting);
    for (const row of rows) {
      const declared = SETTINGS[row.key as SettingKey];
      if (!declared) continue;
      const parsed = declared.schema.safeParse(row.value);
      if (parsed.success) {
        (values as Record<string, unknown>)[row.key] = parsed.data;
      } else {
        // A stored value we cannot parse is a bug, not a reason to refuse to
        // serve: the default is always a safe answer.
        console.warn(`[settings] ignoring unusable value for ${row.key}:`, parsed.error.message);
      }
    }
  } catch (err) {
    // Policy must not be able to take the coordinator down. Reserve and renew
    // both read this.
    console.error("[settings] read failed, using defaults:", err);
    return values;
  }

  cache = { at: Date.now(), values };
  return values;
}

export async function getSetting<K extends SettingKey>(
  db: Database,
  key: K,
): Promise<SettingValue<K>> {
  return (await getSettings(db))[key];
}

export async function setSetting<K extends SettingKey>(
  db: Database,
  key: K,
  value: SettingValue<K>,
  updatedBy: string,
) {
  await db
    .insert(setting)
    .values({ key, value, updatedBy, updatedAt: new Date() })
    .onConflictDoUpdate({
      target: setting.key,
      set: { value, updatedBy, updatedAt: new Date() },
    });
  invalidateSettings();
}

/** Reset to the built-in default by removing the row entirely. */
export async function clearSetting(db: Database, key: SettingKey) {
  await db.delete(setting).where(eq(setting.key, key));
  invalidateSettings();
}
