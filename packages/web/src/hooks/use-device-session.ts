import type { ClientMessage, Display } from "@farm/protocol";
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";
import { isStreamSupported, ScreenRenderer } from "@/lib/screen/renderer";
import { DeviceSession, fallbackStreamUrl, type SessionState } from "@/lib/screen/session";

export interface DeviceSessionApi {
  state: SessionState;
  detail?: string;
  /** Geometry the provider reported, which may lag a rotation by a frame. */
  display: Display | null;
  /** Decoded frame geometry — what the canvas box is actually shaped by. */
  frameSize: { width: number; height: number } | null;
  /** The browser cannot decode this stream: no WebCodecs, or an insecure origin. */
  unsupported: boolean;
  /**
   * Set alongside `unsupported`: a `multipart/x-mixed-replace` URL an `<img>`
   * can show instead. Capped at ~3fps and no input latency budget at all, which
   * is why it is a fallback and not a mode.
   */
  fallbackUrl: string | null;
  /**
   * Asks the device for its clipboard and resolves with the reply — `null`
   * meaning genuinely empty. A request/response rather than a piece of state:
   * reading the same text twice, or reading an empty clipboard, must still be
   * something the caller can react to.
   */
  readClipboard: () => Promise<string | null>;
  send: (message: ClientMessage) => void;
}

/** Long enough for a slow device, short enough to not look hung. */
const CLIPBOARD_TIMEOUT = 5_000;

/**
 * `?fallback=1` forces the degraded path.
 *
 * Otherwise it is unreachable on any machine that can decode the stream, which
 * is every machine a developer is likely to have — and an untestable fallback
 * is one that quietly rots until the day someone actually needs it.
 */
function fallbackForced() {
  return new URLSearchParams(window.location.search).get("fallback") === "1";
}

/**
 * Owns one viewer: the WebSocket, the decoder, and the canvas they feed.
 *
 * A popout window runs this a second time against the same reservation — the
 * provider fans video out per viewer with its own backlog shedding, so two
 * live sessions on one device need no coordination here.
 */
export function useDeviceSession(
  deviceId: string,
  canvasRef: RefObject<HTMLCanvasElement | null>,
  enabled: boolean,
): DeviceSessionApi {
  const [state, setState] = useState<SessionState>("idle");
  const [detail, setDetail] = useState<string | undefined>();
  const [display, setDisplay] = useState<Display | null>(null);
  const [frameSize, setFrameSize] = useState<{ width: number; height: number } | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [fallbackUrl, setFallbackUrl] = useState<string | null>(null);

  const sessionRef = useRef<DeviceSession | null>(null);
  const rendererRef = useRef<ScreenRenderer | null>(null);
  const clipboardWaiters = useRef<((text: string | null) => void)[]>([]);

  const send = useCallback((message: ClientMessage) => {
    sessionRef.current?.send(message);
  }, []);

  const readClipboard = useCallback(
    () =>
      new Promise<string | null>((resolve, reject) => {
        if (!sessionRef.current) {
          reject(new Error("no session"));
          return;
        }
        const timer = setTimeout(() => {
          clipboardWaiters.current = clipboardWaiters.current.filter((w) => w !== waiter);
          reject(new Error("the device did not answer"));
        }, CLIPBOARD_TIMEOUT);
        const waiter = (text: string | null) => {
          clearTimeout(timer);
          resolve(text);
        };
        clipboardWaiters.current.push(waiter);
        sessionRef.current.send({ type: "clipboard.get" });
      }),
    [],
  );

  useEffect(() => {
    if (!enabled) return;

    let disposed = false;
    const requestKeyframe = () => sessionRef.current?.send({ type: "keyframe" });

    const session = new DeviceSession(deviceId, {
      onState: (next, why) => {
        if (disposed) return;
        setState(next);
        setDetail(why);
        if (next === "closed" || next === "idle") {
          rendererRef.current?.destroy();
          rendererRef.current = null;
        }
      },
      onMessage: (message) => {
        if (disposed) return;
        switch (message.type) {
          case "codec": {
            setDisplay(message.display);
            // A re-announcement mid-session is a rotation: same stream, new
            // parameter sets. Swap them in place — tearing the renderer down
            // would blank the canvas and re-ask a question already answered.
            if (rendererRef.current) {
              rendererRef.current.reconfigure(message);
              rendererRef.current.setRenderRotation(message.display.renderRotation);
              requestKeyframe();
              return;
            }
            const canvas = canvasRef.current;
            if (!canvas) return;
            // Check support before building anything: a decoder we cannot
            // configure should surface as the fallback path, not as an
            // exception during the first access unit.
            isStreamSupported(message).then((supported) => {
              if (disposed) return;
              const ok = supported && !fallbackForced();
              if (!ok) {
                setUnsupported(true);
                // Mint the fallback URL only once it is actually needed: it
                // spends a session token, and most browsers never take this
                // path.
                fallbackStreamUrl(deviceId).then(
                  (url) => !disposed && setFallbackUrl(url),
                  (error) => console.warn("[screen] no fallback stream:", error),
                );
                return;
              }
              setUnsupported(false);
              rendererRef.current?.destroy();
              const renderer = new ScreenRenderer(canvas, {
                onSize: (width, height) => setFrameSize({ width, height }),
                onNeedKeyframe: requestKeyframe,
                onError: (error) => console.warn("[screen]", error),
              });
              renderer.configure(message);
              renderer.setRenderRotation(message.display.renderRotation);
              rendererRef.current = renderer;
              requestKeyframe();
            });
            break;
          }
          case "display":
            setDisplay(message.display);
            // iOS rotates without re-encoding: no new parameter sets, no new
            // frame size, and this message is the only thing that says the
            // picture is now sideways.
            rendererRef.current?.setRenderRotation(message.display.renderRotation);
            break;
          case "clipboard": {
            const waiters = clipboardWaiters.current;
            clipboardWaiters.current = [];
            for (const waiter of waiters) waiter(message.text);
            break;
          }
          case "session.closed":
            // Revocation, not a blip — stop the reconnect loop and say why.
            session.markRevoked();
            setDetail(message.reason);
            break;
          case "error":
            console.warn("[session] provider error:", message.message);
            break;
          case "ping":
            session.send({ type: "pong", at: Date.now() });
            break;
          default:
            break;
        }
      },
      onBinary: (frame) => rendererRef.current?.decodeChunk(frame),
    });

    sessionRef.current = session;
    session.open();

    return () => {
      disposed = true;
      session.close();
      sessionRef.current = null;
      rendererRef.current?.destroy();
      rendererRef.current = null;
    };
  }, [deviceId, enabled, canvasRef]);

  return { state, detail, display, frameSize, unsupported, fallbackUrl, readClipboard, send };
}
