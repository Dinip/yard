import type { ClientMessage } from "@yard/protocol";
import { toDevicePoint } from "./rotation";

// Keep synthetic fingers separate from browser pointer IDs and scrcpy's mouse IDs.
const FINGERS = [2147483646, 2147483647] as const;
const REVERSED_FINGERS = [FINGERS[1], FINGERS[0]] as const;
// scrcpy applies contacts one at a time, so bound its temporary midpoint shift.
const ANDROID_RADIUS_STEP = 0.01;

export function attachPinch(
  canvas: HTMLCanvasElement,
  send: (message: ClientMessage) => void,
  rotation: number | null | undefined,
  hasPointers: () => boolean,
  stabilizeSequentialTouches = false,
) {
  let pinch: { x: number; y: number; radius: number; max: number } | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let gestureScale = 1;
  let gesturing = false;
  let reverseMoves = false;

  const emit = (type: "pointer.down" | "pointer.move" | "pointer.up") => {
    if (!pinch) return;
    const fingers = type === "pointer.move" && reverseMoves ? REVERSED_FINGERS : FINGERS;
    for (const pointerId of fingers) {
      const index = pointerId === FINGERS[0] ? 0 : 1;
      send({
        type,
        pointerId,
        at: toDevicePoint(pinch.x + (index === 0 ? -1 : 1) * pinch.radius, pinch.y, rotation),
      });
    }
    if (type === "pointer.move" && stabilizeSequentialTouches) reverseMoves = !reverseMoves;
  };
  const finish = () => {
    clearTimeout(timer);
    emit("pointer.up");
    pinch = undefined;
  };
  const begin = (clientX: number, clientY: number) => {
    const rect = canvas.getBoundingClientRect();
    if (!rect.width || !rect.height || hasPointers()) return false;
    const x = Math.max(0.1, Math.min(0.9, (clientX - rect.left) / rect.width));
    const y = Math.max(0.05, Math.min(0.95, (clientY - rect.top) / rect.height));
    const max = Math.min(x, 1 - x) * 0.95;
    pinch = { x, y, radius: max / 3, max };
    reverseMoves = false;
    emit("pointer.down");
    return true;
  };
  const scale = (factor: number) => {
    if (!pinch || !Number.isFinite(factor) || factor <= 0) return;
    const from = pinch.radius;
    const target = Math.max(0.01, Math.min(pinch.max, from * factor));
    const steps = stabilizeSequentialTouches
      ? Math.max(1, Math.ceil(Math.abs(target - from) / ANDROID_RADIUS_STEP))
      : 1;
    for (let step = 1; step <= steps; step++) {
      pinch.radius = from + ((target - from) * step) / steps;
      emit("pointer.move");
    }
  };
  const wheel = (event: WheelEvent) => {
    event.preventDefault();
    if (gesturing || hasPointers() || !event.deltaY) return;
    if (!pinch && !begin(event.clientX, event.clientY)) return;
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? canvas.clientHeight : 1;
    scale(Math.exp(-Math.max(-100, Math.min(100, event.deltaY * unit)) * 0.005));
    clearTimeout(timer);
    timer = setTimeout(finish, 150);
    if (pinch && (pinch.radius === pinch.max || pinch.radius === 0.01)) finish();
  };
  // Safari exposes trackpad pinches as gesture events instead of Ctrl+wheel.
  const gesture = (event: Event) => {
    event.preventDefault();
    const {
      scale: value,
      clientX,
      clientY,
    } = event as Event & { scale: number; clientX: number; clientY: number };
    if (event.type === "gesturestart") {
      finish();
      gesturing = begin(clientX, clientY);
      gestureScale = 1;
    } else if (event.type === "gestureend") {
      finish();
      gesturing = false;
    } else if (gesturing) {
      scale(value / gestureScale);
      gestureScale = value;
    }
  };
  const cancel = () => {
    finish();
    gesturing = false;
  };
  canvas.addEventListener("wheel", wheel, { passive: false });
  for (const name of ["gesturestart", "gesturechange", "gestureend"]) {
    canvas.addEventListener(name, gesture, { passive: false });
  }
  canvas.addEventListener("pointerdown", cancel);
  window.addEventListener("blur", cancel);
  return () => {
    cancel();
    canvas.removeEventListener("wheel", wheel);
    for (const name of ["gesturestart", "gesturechange", "gestureend"])
      canvas.removeEventListener(name, gesture);
    canvas.removeEventListener("pointerdown", cancel);
    window.removeEventListener("blur", cancel);
  };
}
