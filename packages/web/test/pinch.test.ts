import { expect, test } from "bun:test";
import type { ClientMessage } from "@yard/protocol";
import { attachPinch } from "../src/lib/screen/pinch";

test("wheel and Safari pinch send two rotated fingers and release on interruption", async () => {
  const previousWindow = globalThis.window;
  globalThis.window = new EventTarget() as unknown as Window & typeof globalThis;
  const canvas = Object.assign(new EventTarget(), {
    clientHeight: 600,
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 300, height: 600 }),
  }) as unknown as HTMLCanvasElement;
  const messages: ClientMessage[] = [];
  let touching = false;
  const cleanup = attachPinch(
    canvas,
    (message) => messages.push(message),
    90,
    () => touching,
  );
  const fire = (type: string, data = {}) => {
    const event = Object.assign(new Event(type, { cancelable: true }), {
      clientX: 150,
      clientY: 300,
      deltaY: -30,
      deltaMode: 0,
      ...data,
    });
    canvas.dispatchEvent(event);
    return event;
  };
  try {
    expect(fire("wheel").defaultPrevented).toBe(true);
    expect(messages.map((m) => m.type)).toEqual([
      "pointer.down",
      "pointer.down",
      "pointer.move",
      "pointer.move",
    ]);
    const points = messages.filter((m) => m.type === "pointer.down" || m.type === "pointer.move");
    expect(points[0]?.pointerId).not.toBe(points[1]?.pointerId);
    expect(points[2]?.at.x).toBe(0.5);
    expect(points[2]?.at.y).toBeGreaterThan(points[0]?.at.y ?? 0);
    expect(points[3]?.at.y).toBeLessThan(points[1]?.at.y ?? 1);
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(messages.slice(-2).map((m) => m.type)).toEqual(["pointer.up", "pointer.up"]);
    messages.length = 0;
    touching = true;
    fire("wheel");
    expect(messages).toHaveLength(0);
    touching = false;
    fire("gesturestart", { scale: 1 });
    fire("gesturechange", { scale: 1.2 });
    expect(messages).toHaveLength(4);
    fire("wheel");
    expect(messages).toHaveLength(4);
    fire("gestureend");
    expect(messages.slice(-2).map((m) => m.type)).toEqual(["pointer.up", "pointer.up"]);
    messages.length = 0;
    fire("wheel", { deltaY: 1, deltaMode: 1 });
    fire("pointerdown");
    expect(messages.slice(-2).map((m) => m.type)).toEqual(["pointer.up", "pointer.up"]);
    fire("wheel");
    window.dispatchEvent(new Event("blur"));
    expect(messages.slice(-2).map((m) => m.type)).toEqual(["pointer.up", "pointer.up"]);
    cleanup();
    const count = messages.length;
    fire("wheel");
    await new Promise((resolve) => setTimeout(resolve, 180));
    expect(messages).toHaveLength(count);
  } finally {
    cleanup();
    globalThis.window = previousWindow;
  }
});

test("Android wheel zoom bounds and alternates sequential contact movement", () => {
  const previousWindow = globalThis.window;
  globalThis.window = new EventTarget() as unknown as Window & typeof globalThis;
  const canvas = Object.assign(new EventTarget(), {
    clientHeight: 600,
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 300, height: 600 }),
  }) as unknown as HTMLCanvasElement;
  const messages: ClientMessage[] = [];
  const cleanup = attachPinch(
    canvas,
    (message) => messages.push(message),
    0,
    () => false,
    true,
  );
  const event = Object.assign(new Event("wheel", { cancelable: true }), {
    clientX: 150,
    clientY: 300,
    deltaY: -100,
    deltaMode: 0,
  });

  try {
    canvas.dispatchEvent(event);
    const downs = messages.filter((message) => message.type === "pointer.down");
    const moves = messages.filter((message) => message.type === "pointer.move");
    expect(moves.length).toBeGreaterThan(2);
    let previousRadius = Math.abs(downs[0]!.at.x - 0.5);
    for (let index = 0; index < moves.length; index += 2) {
      const pair = moves.slice(index, index + 2);
      expect(pair.map((message) => message.pointerId)).toEqual(
        index % 4 === 0
          ? [downs[0]?.pointerId, downs[1]?.pointerId]
          : [downs[1]?.pointerId, downs[0]?.pointerId],
      );
      const points = pair
        .filter((message) => message.type === "pointer.move")
        .map((message) => message.at.x);
      expect((points[0]! + points[1]!) / 2).toBeCloseTo(0.5);
      const radius = Math.abs(points[0]! - 0.5);
      expect(Math.abs(radius - previousRadius)).toBeLessThanOrEqual(0.01);
      previousRadius = radius;
    }
  } finally {
    cleanup();
    globalThis.window = previousWindow;
  }
});
