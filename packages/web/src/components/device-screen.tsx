import type { ClientMessage } from "@yard/protocol";
import { Loader2, MonitorOff } from "lucide-react";
import { type PointerEvent as ReactPointerEvent, useCallback, useRef } from "react";
import type { DeviceSessionApi } from "@/hooks/use-device-session";
import { toDevicePoint } from "@/lib/screen/rotation";
import { cn } from "@/lib/utils";

/**
 * The live screen and the input surface over it.
 *
 * Coordinates go out normalised 0..1, never pixels: the browser needs no
 * knowledge of the device's true resolution, and a rotation mid-gesture cannot
 * desynchronise an in-flight event. down/move/up stay three separate messages
 * because the provider's pointer state machine is what tells a drag from a tap.
 */
export function DeviceScreen({
  session,
  canvasRef,
  className,
  interactive = true,
}: {
  session: DeviceSessionApi;
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  className?: string;
  interactive?: boolean;
}) {
  const { send, frameSize, display, state, unsupported, fallbackUrl, detail } = session;
  const activePointers = useRef(new Map<number, { x: number; y: number }>());

  // The canvas is in viewer space; the device's HID surface is in device
  // space, and on iOS the renderer has turned the picture between the two.
  const renderRotation = display?.renderRotation;
  const at = useCallback(
    (event: ReactPointerEvent<HTMLCanvasElement>) => {
      const rect = event.currentTarget.getBoundingClientRect();
      const point = toDevicePoint(
        clamp01((event.clientX - rect.left) / rect.width),
        clamp01((event.clientY - rect.top) / rect.height),
        renderRotation,
      );
      return { x: clamp01(point.x), y: clamp01(point.y) };
    },
    [renderRotation],
  );

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!interactive) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    event.currentTarget.focus();
    const point = at(event);
    activePointers.current.set(event.pointerId, point);
    send({ type: "pointer.down", pointerId: event.pointerId, at: point });
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!interactive || !activePointers.current.has(event.pointerId)) return;
    const point = at(event);
    activePointers.current.set(event.pointerId, point);
    send({ type: "pointer.move", pointerId: event.pointerId, at: point });
  };

  const endPointer = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    if (!activePointers.current.delete(event.pointerId)) return;
    send({ type: "pointer.up", pointerId: event.pointerId, at: at(event) });
  };

  const onLostPointerCapture = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const point = activePointers.current.get(event.pointerId);
    if (!point) return;
    activePointers.current.delete(event.pointerId);
    // Safari can revoke capture when an edge swipe leaves the browser's input
    // surface. Its lost-capture event does not carry reliable coordinates, so
    // lift at the last point we sent instead of leaving a contact on the device.
    send({ type: "pointer.up", pointerId: event.pointerId, at: point });
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLCanvasElement>) => {
    if (!interactive) return;
    const message = keyMessage(event, true);
    if (!message) return;
    event.preventDefault();
    send(message);
  };

  const onKeyUp = (event: React.KeyboardEvent<HTMLCanvasElement>) => {
    if (!interactive) return;
    // Printable characters were already committed as text on keydown.
    if (event.key.length === 1) return;
    const message = keyMessage(event, false);
    if (!message) return;
    event.preventDefault();
    send(message);
  };

  const onPaste = (event: React.ClipboardEvent<HTMLCanvasElement>) => {
    if (!interactive) return;
    const text = event.clipboardData.getData("text");
    if (!text) return;
    event.preventDefault();
    send({ type: "text", text });
  };

  const aspect = frameSize
    ? frameSize.width / frameSize.height
    : display?.width && display?.height
      ? display.width / display.height
      : 9 / 16;

  return (
    // The picture is laid out *out of flow*, so this box contributes no
    // intrinsic height of its own. That is load-bearing: an aspect-ratio box in
    // normal flow derives a height from the available width, and a tall thin
    // device in a wide column asked for more height than the viewport had —
    // which is how the whole page came to overflow. Nothing above it now has to
    // be given a definite height to hold it in.
    <div className={cn("relative min-h-0", className)}>
      <div className="absolute inset-0 flex items-center justify-center">
        <div
          className="relative flex max-h-full max-w-full items-center justify-center"
          style={{ aspectRatio: aspect }}
        >
          <canvas
            ref={canvasRef}
            tabIndex={0}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={endPointer}
            onPointerCancel={endPointer}
            onLostPointerCapture={onLostPointerCapture}
            onKeyDown={onKeyDown}
            onKeyUp={onKeyUp}
            onPaste={onPaste}
            onContextMenu={(event) => event.preventDefault()}
            className={cn(
              "h-full max-h-full w-full max-w-full rounded-md bg-black outline-none",
              "ring-offset-2 ring-offset-background focus-visible:ring-2 focus-visible:ring-ring",
              interactive ? "touch-none" : "pointer-events-none",
            )}
          />
          {/*
          The fallback: still frames over multipart, ~3fps, no decode involved.
          It sits above the canvas rather than replacing it so the input surface
          underneath keeps working — a degraded picture is still a usable device.
        */}
          {unsupported && fallbackUrl && (
            <img
              src={fallbackUrl}
              alt="Device screen (fallback stream)"
              className="pointer-events-none absolute inset-0 h-full w-full rounded-md object-contain"
            />
          )}

          {state !== "open" || (unsupported && !fallbackUrl) ? (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 rounded-md bg-black/70 text-center text-sm text-white">
              {unsupported ? (
                <>
                  <MonitorOff className="size-6" />
                  <p className="max-w-xs">
                    This browser cannot decode the device's stream. WebCodecs needs a secure context
                    and a matching hardware decoder.
                  </p>
                </>
              ) : (
                <>
                  <Loader2 className="size-6 animate-spin" />
                  <p>{state === "closed" ? (detail ?? "Session closed") : "Connecting…"}</p>
                </>
              )}
            </div>
          ) : null}

          {/* Say so, rather than letting a jerky picture look like a slow farm. */}
          {unsupported && fallbackUrl && (
            <span className="absolute top-2 left-2 rounded bg-black/70 px-2 py-1 text-white text-xs">
              Fallback stream — ~3 fps, no hardware decoder here
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function clamp01(value: number) {
  return Math.min(1, Math.max(0, value));
}

/**
 * Printable characters go as `text` — that is what carries IME output and
 * anything a key name cannot express. Everything else goes as a named key the
 * backend maps to a real keycode.
 */
function keyMessage(event: React.KeyboardEvent, down: boolean): ClientMessage | null {
  if (event.metaKey || event.ctrlKey || event.altKey) return null;
  if (event.key.length === 1) {
    return down ? { type: "text", text: event.key } : null;
  }
  const name = NAMED_KEYS.get(event.key);
  if (!name) return null;
  return { type: "key", key: name, down };
}

/**
 * Keyboard key → the name sent on the wire. Mostly identity.
 *
 * Home and End are the exception, and the reason this is a map rather than a
 * set: `Home` on the wire is the *hardware* button, so sending the keyboard's
 * Home key under that name threw the device to the launcher in the middle of
 * typing. `MoveHome`/`MoveEnd` are the text-editing pair — Android
 * `KEYCODE_MOVE_HOME`, iOS HID 0x4A.
 */
const NAMED_KEYS = new Map([
  ["Enter", "Enter"],
  ["Backspace", "Backspace"],
  ["Tab", "Tab"],
  ["Escape", "Escape"],
  ["ArrowUp", "ArrowUp"],
  ["ArrowDown", "ArrowDown"],
  ["ArrowLeft", "ArrowLeft"],
  ["ArrowRight", "ArrowRight"],
  ["Home", "MoveHome"],
  ["End", "MoveEnd"],
  ["PageUp", "PageUp"],
  ["PageDown", "PageDown"],
  ["Delete", "Delete"],
]);
