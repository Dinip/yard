import { z } from "zod";
import { named } from "./registry.ts";

/** Epoch milliseconds. Chosen over ISO strings so Rust needs no date crate on the wire. */
export const Timestamp = z.number().int();

export const Platform = named("Platform", z.enum(["ios", "android"]));

export const DeviceStatus = named(
  "DeviceStatus",
  z.enum(["absent", "present", "preparing", "ready", "busy", "unhealthy"]),
);

export const Display = named(
  "Display",
  z.object({
    width: z.number().int(),
    height: z.number().int(),
    /** Backing-store scale, e.g. 3 on a modern iPhone. Drives the popout window size. */
    scale: z.number().optional(),
    rotation: z.number().int().optional(),
  }),
);

export const Battery = named(
  "Battery",
  z.object({
    /** 0..1 */
    level: z.number().optional(),
    state: z.string().optional(),
  }),
);

/**
 * A provider's view of one device. Sent whole on `hello` and whenever anything
 * material changes — the coordinator reconciles rather than applying deltas, so
 * a missed message cannot leave the two sides permanently disagreeing.
 */
export const DeviceSnapshot = named(
  "DeviceSnapshot",
  z.object({
    /** udid (iOS) or serial (Android). */
    id: z.string(),
    platform: Platform,
    status: DeviceStatus,
    name: z.string().optional(),
    model: z.string().optional(),
    manufacturer: z.string().optional(),
    osVersion: z.string().optional(),
    abi: z.string().optional(),
    sdk: z.number().int().optional(),
    display: Display.optional(),
    battery: Battery.optional(),
    /** Handed straight to the browser's VideoDecoder, e.g. "hev1.1.6.L93.B0". */
    streamCodec: z.string().optional(),
    /** Set while an adb transport is exposed for remote debugging. */
    adbPort: z.number().int().optional(),
    note: z.string().optional(),
  }),
);

export const AppInfo = named(
  "AppInfo",
  z.object({
    id: z.string(),
    name: z.string().optional(),
    version: z.string().optional(),
    system: z.boolean().optional(),
  }),
);

export type Platform = z.infer<typeof Platform>;
export type DeviceStatus = z.infer<typeof DeviceStatus>;
export type Display = z.infer<typeof Display>;
export type Battery = z.infer<typeof Battery>;
export type DeviceSnapshot = z.infer<typeof DeviceSnapshot>;
export type AppInfo = z.infer<typeof AppInfo>;
