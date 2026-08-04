import { z } from "zod";
import { Display, Timestamp } from "./common.ts";
import { named } from "./registry.ts";

/**
 * Session plane: browser ↔ provider, direct WSS at
 * `wss://<publicBaseUrl>/s/<deviceId>?token=<jwt>`.
 *
 * Text frames carry the JSON messages below. **Binary frames are video access
 * units** and are not described here: they are `[type byte][AU]`, where the
 * type byte is 0 = key, 1 = delta, 2 = key-with-reset. That framing is what
 * stf-ios-provider's renderer already speaks; see its frontend/INTEGRATION.md.
 */

/**
 * Normalised 0..1 coordinates, not pixels. The browser does not need to know
 * the device's true resolution, and a mid-session rotation or display change
 * cannot desynchronise an in-flight event.
 */
export const PointerPoint = named(
  "PointerPoint",
  z.object({
    x: z.number(),
    y: z.number(),
  }),
);

export const ClientMessage = named(
  "ClientMessage",
  z.discriminatedUnion("type", [
    /**
     * `pointerId` lets the provider's Pointer state machine distinguish a drag
     * from a tap — the regression stf-ios-provider's control.rs documents at
     * length. Do not collapse down/move/up into a single "tap" message.
     */
    z.object({
      type: z.literal("pointer.down"),
      pointerId: z.number().int(),
      at: PointerPoint,
    }),
    z.object({
      type: z.literal("pointer.move"),
      pointerId: z.number().int(),
      at: PointerPoint,
    }),
    z.object({
      type: z.literal("pointer.up"),
      pointerId: z.number().int(),
      at: PointerPoint,
    }),
    /** Physical/named key, e.g. "Enter", "Backspace", "Home". */
    z.object({
      type: z.literal("key"),
      key: z.string(),
      down: z.boolean(),
    }),
    /** Committed text, including IME output and paste. */
    z.object({ type: z.literal("text"), text: z.string() }),
    z.object({ type: z.literal("clipboard.get") }),
    z.object({ type: z.literal("clipboard.set"), text: z.string() }),
    z.object({ type: z.literal("rotate"), degrees: z.number().int() }),
    /** Ask for an IDR — used on first paint and after a decoder reset. */
    z.object({ type: z.literal("keyframe") }),
    z.object({ type: z.literal("pong"), at: Timestamp }),
  ]),
);

export const ServerMessage = named(
  "ServerMessage",
  z.discriminatedUnion("type", [
    /**
     * Sent once before any binary frame. `description` is base64 hvcC (iOS) or
     * avcC (Android); the renderer feeds both straight to VideoDecoder.configure
     * without caring which codec it got.
     */
    z.object({
      type: z.literal("codec"),
      codec: z.string(),
      description: z.string().optional(),
      display: Display,
    }),
    z.object({ type: z.literal("display"), display: Display }),
    z.object({ type: z.literal("clipboard"), text: z.string().nullable() }),
    z.object({
      type: z.literal("install.progress"),
      /** 0..1, or absent while the provider cannot estimate. */
      progress: z.number().optional(),
      stage: z.string(),
    }),
    z.object({
      type: z.literal("install.result"),
      ok: z.boolean(),
      appId: z.string().optional(),
      error: z.string().optional(),
    }),
    /** The coordinator revoked the reservation, or it expired. */
    z.object({ type: z.literal("session.closed"), reason: z.string() }),
    z.object({ type: z.literal("error"), message: z.string() }),
    z.object({ type: z.literal("ping"), at: Timestamp }),
  ]),
);

export type PointerPoint = z.infer<typeof PointerPoint>;
export type ClientMessage = z.infer<typeof ClientMessage>;
export type ServerMessage = z.infer<typeof ServerMessage>;

/** Binary video frame type byte. Mirrored in Rust as `AuKind`. */
export const AU_KEY = 0;
export const AU_DELTA = 1;
export const AU_KEY_RESET = 2;
