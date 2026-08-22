/**
 * Codec-agnostic WebCodecs screen renderer.
 *
 * Ported from `stf-ios-provider/src/frontend/hevc-screen.js` and generalised:
 * the codec and its parameter sets come from the `{type:"codec"}` handshake
 * (`hev1.*`+hvcC on iOS, `avc1.*`+avcC on Android) rather than being assumed,
 * so nothing here knows which backend it is talking to.
 *
 * Framing — see packages/protocol/src/session.ts:
 *   text  : {type:"codec", codec, description, display} once, before any frame
 *   binary: [typeByte][accessUnit], 0 = key, 1 = delta, 2 = key WITH RESET
 *
 * Type 2 means the provider saw an upstream drop or shed frames for this
 * viewer. The decoder is rebuilt before decoding it: hardware decoders render
 * torn frames from a stale reference without ever firing `error`, so a silent
 * wrong picture — not a visible failure — is what this guards against.
 *
 * `description` carries the parameter sets out-of-band, which keeps payloads
 * length-prefixed NALUs. The Annex-B start-code path tears under motion in
 * Chrome; if a backend ever ships without a description we configure anyway,
 * but that is the degraded path, not the intended one.
 */

import { AU_DELTA, AU_KEY, AU_KEY_RESET } from "@yard/protocol";
import { normalizeRotation } from "@/lib/screen/rotation";

export interface RendererOptions {
  /** Hide the loader on first paint. */
  onFirstFrame?: () => void;
  onError?: (error: unknown) => void;
  /** Decoded frame geometry, for sizing the canvas box. */
  onSize?: (width: number, height: number) => void;
  /** The decoder lost its anchor — ask the provider for an IDR. */
  onNeedKeyframe?: () => void;
  /**
   * Undo iOS's motion resolution-collapse (see detectContentCrop). Defaults to
   * on for HEVC only: it is an iOS encoder behaviour, and the per-frame
   * readback it costs is pure waste on Android.
   */
  compensate?: boolean;
}

export interface CodecInfo {
  codec: string;
  /** base64 hvcC or avcC. */
  description?: string;
}

function base64ToBytes(b64: string): Uint8Array {
  const raw = atob(b64);
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

function configFor(info: CodecInfo): VideoDecoderConfig {
  return {
    codec: info.codec,
    ...(info.description ? { description: base64ToBytes(info.description) } : {}),
    hardwareAcceleration: "prefer-hardware",
    optimizeForLatency: true,
  };
}

/**
 * Whether this browser can decode the stream at all. A `false` here is what
 * drives the MJPEG fallback — WebCodecs is also gated on a secure context, so
 * plain http from a non-loopback origin fails this too.
 */
export async function isStreamSupported(info: CodecInfo): Promise<boolean> {
  if (typeof VideoDecoder === "undefined") return false;
  try {
    const { supported } = await VideoDecoder.isConfigSupported(configFor(info));
    return supported === true;
  } catch {
    return false;
  }
}

export class ScreenRenderer {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly opts: RendererOptions;

  private decoder: VideoDecoder | null = null;
  private config: VideoDecoderConfig | null = null;
  private compensate = false;
  private needsResync = false;
  private gotKey = false;
  private timestamp = 0;
  private sawFirstFrame = false;

  // rAF coalescing: decode output can outpace the display, and drawing frames
  // nobody sees just burns GPU.
  private pendingFrame: VideoFrame | null = null;
  private rafScheduled = false;

  // Collapse-detection scratch surface (see detectContentCrop).
  private readonly det = document.createElement("canvas");
  private readonly detCtx: CanvasRenderingContext2D;
  private cropSrcW = 0;
  private cropSrcH = 0;

  private lastW = 0;
  private lastH = 0;
  private renderRotation = 0;

  constructor(canvas: HTMLCanvasElement, opts: RendererOptions = {}) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
    const detCtx = this.det.getContext("2d", { willReadFrequently: true });
    if (!ctx || !detCtx) throw new Error("2d canvas context unavailable");
    this.ctx = ctx;
    this.detCtx = detCtx;
    this.opts = opts;
  }

  configure(info: CodecInfo) {
    this.config = configFor(info);
    this.compensate = shouldCompensate(this.opts, info.codec);
    this.gotKey = false;
    this.sawFirstFrame = false;
    this.buildDecoder();
  }

  /**
   * Swap in new parameter sets mid-stream — what a rotation produces.
   *
   * Distinct from `configure` in what it leaves alone: the canvas keeps its
   * last painted frame until the new keyframe lands, and `sawFirstFrame` stays
   * set so the loader does not flash back over a picture that is already
   * there. Rebuilding the whole renderer instead would drop both, and re-run
   * `isStreamSupported` for a codec that is by definition still supported.
   */
  reconfigure(info: CodecInfo) {
    this.config = configFor(info);
    this.compensate = shouldCompensate(this.opts, info.codec);
    // Nothing decoded against the old description can be referenced by the new
    // stream, so hold everything until the provider's reset keyframe.
    this.gotKey = false;
    this.buildDecoder();
  }

  private buildDecoder() {
    if (!this.config) return;
    closeQuietly(this.decoder);
    this.decoder = new VideoDecoder({
      output: (frame) => this.onFrame(frame),
      error: (err) => {
        // Don't tear down: the next keyframe re-anchors us, and the provider
        // sends a recovery IDR when a viewer asks for one.
        this.needsResync = true;
        console.warn("[screen] decoder error", err.message);
        this.opts.onNeedKeyframe?.();
      },
    });
    this.decoder.configure(this.config);
    this.needsResync = false;
  }

  /** Feed one binary WebSocket frame: `[typeByte][accessUnit]`. */
  decodeChunk(bytes: Uint8Array) {
    if (!this.decoder || !this.config) return;
    const kind = bytes[0];
    const data = bytes.subarray(1);

    if (kind === AU_KEY_RESET) {
      this.buildDecoder();
      this.gotKey = true;
    } else if (kind === AU_KEY) {
      this.gotKey = true;
      if (this.needsResync) this.buildDecoder();
    }

    if (!this.gotKey || this.needsResync) return;

    if (this.decoder.state !== "configured") {
      this.buildDecoder();
      this.needsResync = true;
      this.opts.onNeedKeyframe?.();
      return;
    }

    try {
      this.decoder.decode(
        new EncodedVideoChunk({
          type: kind === AU_DELTA ? "delta" : "key",
          timestamp: this.timestamp,
          data,
        }),
      );
      // Nominal 60fps spacing. The stream is not CFR, but WebCodecs only uses
      // the timestamp for ordering and monotonic is all that is required.
      this.timestamp += 16_666;
    } catch {
      this.needsResync = true;
      this.opts.onNeedKeyframe?.();
    }
  }

  private onFrame(frame: VideoFrame) {
    this.pendingFrame?.close();
    this.pendingFrame = frame;
    if (!this.rafScheduled) {
      this.rafScheduled = true;
      requestAnimationFrame(() => this.drawPending());
    }
  }

  private drawPending() {
    this.rafScheduled = false;
    const frame = this.pendingFrame;
    this.pendingFrame = null;
    if (!frame) return;
    try {
      if (this.compensate) this.detectContentCrop(frame);
      this.drawFrame(frame);
      if (!this.sawFirstFrame) {
        this.sawFirstFrame = true;
        this.opts.onFirstFrame?.();
      }
    } catch (error) {
      this.opts.onError?.(error);
    } finally {
      frame.close();
    }
  }

  /*
   * Under sustained motion iOS drops to a smaller capture resolution rendered
   * into the TOP-LEFT of the fixed buffer, the rest flat gray (Y=Cb=Cr=128) —
   * the "screen shrinks into a corner while swiping" artifact. It is in the
   * bitstream, a device-side decision under the encoder's bitrate cap, and
   * cannot be prevented from our side.
   *
   * But the shrunk region is the whole screen, just in fewer pixels. Finding
   * that content rectangle and stretching it back to fill the canvas turns the
   * collapse into a momentary softness dip instead of a jarring corner shrink.
   *
   * Runs per-frame on the RAW decoded frame, never the already-compensated
   * canvas — feeding the output back in would oscillate. Per-frame rather than
   * throttled+cached because the stream alternates between collapsed and
   * full-res faster than any throttle, and applying a stale crop to a full
   * frame zooms it into its own top-left corner.
   */
  private detectContentCrop(frame: VideoFrame) {
    const fw = frame.displayWidth;
    const fh = frame.displayHeight;
    if (!fw || !fh) return;

    const DET_H = 344;
    const DW = Math.max(8, Math.round((DET_H * fw) / fh));
    this.det.width = DW;
    this.det.height = DET_H;
    try {
      this.detCtx.drawImage(frame, 0, 0, DW, DET_H);
      const d = this.detCtx.getImageData(0, 0, DW, DET_H).data;
      const isGray = (x: number, y: number) => {
        const i = (y * DW + x) * 4;
        return (
          Math.abs((d[i] ?? 0) - 128) < 6 &&
          Math.abs((d[i + 1] ?? 0) - 128) < 6 &&
          Math.abs((d[i + 2] ?? 0) - 128) < 6
        );
      };

      // Last column / row still holding content (<60% gray padding).
      let cr = 0;
      for (let x = 0; x < DW; x++) {
        let g = 0;
        let n = 0;
        for (let y = 0; y < DET_H; y += 4) {
          n++;
          if (isGray(x, y)) g++;
        }
        if (g / n < 0.6) cr = x;
      }
      let cb = 0;
      for (let y = 0; y < DET_H; y++) {
        let g = 0;
        let n = 0;
        for (let x = 0; x < DW; x += 4) {
          n++;
          if (isGray(x, y)) g++;
        }
        if (g / n < 0.6) cb = y;
      }

      const cw = Math.round((cr / DW) * fw);
      const ch = Math.round((cb / DET_H) * fh);
      // Only compensate on a clear collapse in both dimensions; anything else
      // is a normal frame and must be drawn untouched.
      if (cw > fw * 0.2 && cw < fw * 0.92 && ch > fh * 0.2 && ch < fh * 0.92) {
        this.cropSrcW = cw;
        this.cropSrcH = ch;
      } else {
        this.cropSrcW = 0;
        this.cropSrcH = 0;
      }
    } catch {
      // Transient readback failure — keep the previous crop.
    }
  }

  /**
   * How far to turn the picture, clockwise, before drawing it.
   *
   * Only iOS ever asks for this — see `lib/screen/rotation.ts`. Rotating here
   * rather than with a CSS transform keeps the canvas the shape of what is
   * actually on it, so the box sizing and the pointer mapping both follow from
   * one number instead of two that can disagree.
   */
  setRenderRotation(degrees: number | null | undefined) {
    const next = normalizeRotation(degrees);
    if (next === this.renderRotation) return;
    this.renderRotation = next;
    // The canvas is about to change shape; make the next frame report its size
    // even if the decoded dimensions did not move.
    this.lastW = 0;
    this.lastH = 0;
  }

  private drawFrame(frame: VideoFrame) {
    const fw = frame.displayWidth;
    const fh = frame.displayHeight;
    const swap = this.renderRotation % 180 !== 0;
    const cw = swap ? fh : fw;
    const ch = swap ? fw : fh;

    if (this.canvas.width !== cw || this.canvas.height !== ch) {
      this.canvas.width = cw;
      this.canvas.height = ch;
    }
    if (cw !== this.lastW || ch !== this.lastH) {
      this.lastW = cw;
      this.lastH = ch;
      this.opts.onSize?.(cw, ch);
    }

    const sw = this.cropSrcW || fw;
    const sh = this.cropSrcH || fh;
    if (this.renderRotation === 0) {
      this.ctx.drawImage(frame, 0, 0, sw, sh, 0, 0, fw, fh);
      return;
    }
    this.ctx.save();
    this.ctx.translate(cw / 2, ch / 2);
    this.ctx.rotate((this.renderRotation * Math.PI) / 180);
    this.ctx.drawImage(frame, 0, 0, sw, sh, -fw / 2, -fh / 2, fw, fh);
    this.ctx.restore();
  }

  destroy() {
    this.pendingFrame?.close();
    this.pendingFrame = null;
    closeQuietly(this.decoder);
    this.decoder = null;
  }
}

/** HEVC means iOS, which is the only backend with the collapse behaviour. */
function shouldCompensate(opts: RendererOptions, codec: string): boolean {
  return opts.compensate ?? /^(hev|hvc)/i.test(codec);
}

/** `close()` throws on an already-closed decoder, which is not an error here. */
function closeQuietly(decoder: VideoDecoder | null) {
  try {
    decoder?.close();
  } catch {
    /* already closed */
  }
}
