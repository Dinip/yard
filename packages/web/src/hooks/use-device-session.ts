import type { ClientMessage, Display } from "@farm/protocol";
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";
import { isStreamSupported, ScreenRenderer } from "@/lib/screen/renderer";
import { DeviceSession, type SessionState } from "@/lib/screen/session";

export interface DeviceSessionApi {
  state: SessionState;
  detail?: string;
  /** Geometry the provider reported, which may lag a rotation by a frame. */
  display: Display | null;
  /** Decoded frame geometry — what the canvas box is actually shaped by. */
  frameSize: { width: number; height: number } | null;
  /** The browser cannot decode this stream: no WebCodecs, or an insecure origin. */
  unsupported: boolean;
  /** Last clipboard payload the device sent back. */
  clipboard: string | null;
  send: (message: ClientMessage) => void;
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
  const [clipboard, setClipboard] = useState<string | null>(null);

  const sessionRef = useRef<DeviceSession | null>(null);
  const rendererRef = useRef<ScreenRenderer | null>(null);

  const send = useCallback((message: ClientMessage) => {
    sessionRef.current?.send(message);
  }, []);

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
            const canvas = canvasRef.current;
            if (!canvas) return;
            // Check support before building anything: a decoder we cannot
            // configure should surface as the fallback path, not as an
            // exception during the first access unit.
            isStreamSupported(message).then((ok) => {
              if (disposed || !ok) {
                setUnsupported(!ok);
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
              rendererRef.current = renderer;
              requestKeyframe();
            });
            break;
          }
          case "display":
            setDisplay(message.display);
            break;
          case "clipboard":
            setClipboard(message.text);
            break;
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

  return { state, detail, display, frameSize, unsupported, clipboard, send };
}
