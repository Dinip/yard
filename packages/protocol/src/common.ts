import { z } from "zod";
import { named } from "./registry.ts";

/** Epoch milliseconds. Chosen over ISO strings so Rust needs no date crate on the wire. */
export const Timestamp = z.number().int();

export const Platform = named("Platform", z.enum(["ios", "android"]));

export const DeviceStatus = named(
  "DeviceStatus",
  z.enum(["absent", "present", "preparing", "ready", "busy", "cleaning", "unhealthy"]),
);

export const Display = named(
  "Display",
  z.object({
    width: z.number().int(),
    height: z.number().int(),
    /** Backing-store scale, e.g. 3 on a modern iPhone. Drives the popout window size. */
    scale: z.number().optional(),
    /** The device's own orientation, in degrees: 0, 90, 180, 270. */
    rotation: z.number().int().optional(),
    /**
     * How far the *viewer* must rotate the decoded picture, clockwise, to make
     * it upright — and it is not the same question as `rotation`.
     *
     * Android's encoder follows the device: a rotated phone streams swapped
     * dimensions and the picture is already upright, so this stays 0. iOS
     * captures its native portrait buffer whatever the orientation and draws
     * the rotated UI *inside* it, so the frames stay 9:16 with the content
     * sideways and only the client can put it right. Inferring this from
     * `rotation` versus the frame's aspect would be guessing at which backend
     * is on the other end; this states it.
     */
    renderRotation: z.number().int().optional(),
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
    /**
     * Identity a tester needs to file a bug against the right device. All of it
     * comes out of the `getprop` round-trip the provider already makes, so none
     * of these costs an extra call.
     */
    serial: z.string().optional(),
    brand: z.string().optional(),
    buildId: z.string().optional(),
    securityPatch: z.string().optional(),
    abiList: z.string().optional(),
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

/** A provider-retained preload, reported as desired state rather than bytes. */
export const PreloadInfo = named(
  "PreloadInfo",
  z.object({
    deviceId: z.string(),
    appId: z.string(),
    platform: Platform,
    filename: z.string(),
    size: z.number().int(),
    sha256: z.string(),
  }),
);

export type Platform = z.infer<typeof Platform>;
export type DeviceStatus = z.infer<typeof DeviceStatus>;
export type Display = z.infer<typeof Display>;
export type Battery = z.infer<typeof Battery>;
export type DeviceSnapshot = z.infer<typeof DeviceSnapshot>;
export type AppInfo = z.infer<typeof AppInfo>;
export type PreloadInfo = z.infer<typeof PreloadInfo>;
