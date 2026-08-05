/**
 * The renderer's decode state machine, driven against a stub VideoDecoder.
 *
 * The mock backend's video is deliberately not decodable — synthesising a real
 * stream would mean shipping an encoder to serve a fixture — so this is where
 * the recovery behaviour gets verified without hardware. What matters here is
 * exactly what the field failures were: a type-2 access unit must rebuild the
 * decoder before it is decoded, and deltas must never reach a decoder that has
 * no keyframe to reference. Both produce silently torn pictures, not errors.
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { AU_DELTA, AU_KEY, AU_KEY_RESET } from "@farm/protocol";

interface DecodedChunk {
  type: "key" | "delta";
}

const decoders: StubDecoder[] = [];

class StubDecoder {
  state: "configured" | "closed" | "unconfigured" = "unconfigured";
  chunks: DecodedChunk[] = [];
  config: unknown = null;
  readonly onError: (error: { message: string }) => void;

  readonly onOutput: (frame: unknown) => void;

  constructor(init: { output: (frame: unknown) => void; error: (error: unknown) => void }) {
    this.onError = init.error as (error: { message: string }) => void;
    this.onOutput = init.output;
    decoders.push(this);
  }

  configure(config: unknown) {
    this.config = config;
    this.state = "configured";
  }

  decode(chunk: DecodedChunk) {
    if (this.state !== "configured") throw new Error("not configured");
    this.chunks.push(chunk);
    // A portrait frame, which is what iOS emits whichever way up the phone is.
    this.onOutput({ displayWidth: 1080, displayHeight: 2400, close() {} });
  }

  close() {
    if (this.state === "closed") throw new Error("already closed");
    this.state = "closed";
  }

  static isConfigSupported(config: { codec: string }) {
    return Promise.resolve({ supported: config.codec !== "unsupported.codec" });
  }
}

const stubContext = {
  drawImage() {},
  getImageData: () => ({ data: new Uint8ClampedArray(4) }),
  save() {},
  restore() {},
  translate() {},
  rotate() {},
};

function stubCanvas() {
  return { width: 0, height: 0, getContext: () => stubContext } as unknown as HTMLCanvasElement;
}

function au(kind: number, byte = 0xaa) {
  return new Uint8Array([kind, byte, byte, byte]);
}

let ScreenRenderer: typeof import("../src/lib/screen/renderer.ts").ScreenRenderer;
let isStreamSupported: typeof import("../src/lib/screen/renderer.ts").isStreamSupported;

beforeEach(async () => {
  decoders.length = 0;
  Object.assign(globalThis, {
    VideoDecoder: StubDecoder,
    EncodedVideoChunk: class {
      type: string;
      constructor(init: { type: string }) {
        this.type = init.type;
      }
    },
    document: { createElement: () => stubCanvas() },
    // Synchronous, so a decoded frame reaches the canvas within the test.
    requestAnimationFrame: (cb: (t: number) => void) => {
      cb(0);
      return 0;
    },
  });
  ({ ScreenRenderer, isStreamSupported } = await import("../src/lib/screen/renderer.ts"));
});

afterEach(() => {
  for (const key of ["VideoDecoder", "EncodedVideoChunk", "document", "requestAnimationFrame"]) {
    delete (globalThis as Record<string, unknown>)[key];
  }
});

function renderer() {
  const instance = new ScreenRenderer(stubCanvas());
  instance.configure({ codec: "avc1.640028", description: btoa("\x01\x64\x00\x28") });
  return instance;
}

describe("ScreenRenderer", () => {
  test("takes codec and parameter sets from the handshake, whatever the codec", () => {
    renderer();
    const config = decoders[0]?.config as { codec: string; description: Uint8Array };
    expect(config.codec).toBe("avc1.640028");
    expect(Array.from(config.description)).toEqual([0x01, 0x64, 0x00, 0x28]);
  });

  test("drops deltas until the first keyframe arrives", () => {
    const instance = renderer();
    instance.decodeChunk(au(AU_DELTA));
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders[0]?.chunks).toHaveLength(0);

    instance.decodeChunk(au(AU_KEY));
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders[0]?.chunks.map((c) => c.type)).toEqual(["key", "delta"]);
  });

  test("key-with-reset rebuilds the decoder before decoding", () => {
    const instance = renderer();
    instance.decodeChunk(au(AU_KEY));
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders).toHaveLength(1);

    instance.decodeChunk(au(AU_KEY_RESET));
    // A second decoder, and the reset AU landed in it rather than in the one
    // holding the stale reference.
    expect(decoders).toHaveLength(2);
    expect(decoders[0]?.state).toBe("closed");
    expect(decoders[1]?.chunks.map((c) => c.type)).toEqual(["key"]);
  });

  test("a decoder error asks for a keyframe and resyncs on the next one", () => {
    let keyframesRequested = 0;
    const instance = new ScreenRenderer(stubCanvas(), {
      onNeedKeyframe: () => keyframesRequested++,
    });
    instance.configure({ codec: "avc1.640028" });
    instance.decodeChunk(au(AU_KEY));

    decoders[0]?.onError({ message: "lost reference" });
    expect(keyframesRequested).toBe(1);

    // Deltas after the error must not be decoded: they reference a frame this
    // decoder no longer holds.
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders[0]?.chunks).toHaveLength(1);

    instance.decodeChunk(au(AU_KEY));
    expect(decoders).toHaveLength(2);
    expect(decoders[1]?.chunks.map((c) => c.type)).toEqual(["key"]);
  });

  test("configuring without a description still builds a decoder", () => {
    const instance = new ScreenRenderer(stubCanvas());
    instance.configure({ codec: "hev1.1.6.L93.B0" });
    expect(decoders[0]?.config).not.toHaveProperty("description");
    instance.decodeChunk(au(AU_KEY));
    expect(decoders[0]?.chunks).toHaveLength(1);
  });

  test("reconfigure swaps parameter sets and waits for the reset keyframe", () => {
    const instance = renderer();
    instance.decodeChunk(au(AU_KEY));
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders).toHaveLength(1);

    // What a rotation looks like: same codec, new SPS/PPS.
    instance.reconfigure({ codec: "avc1.640028", description: btoa("\x01\x64\x00\x33") });
    const config = decoders[1]?.config as { description: Uint8Array };
    expect(Array.from(config.description)).toEqual([0x01, 0x64, 0x00, 0x33]);

    // Deltas from the old geometry must not reach the new decoder.
    instance.decodeChunk(au(AU_DELTA));
    expect(decoders[1]?.chunks).toHaveLength(0);

    instance.decodeChunk(au(AU_KEY_RESET));
    expect(decoders[2]?.chunks.map((c) => c.type)).toEqual(["key"]);
  });

  test("a render rotation turns the picture and reshapes the canvas", () => {
    const sizes: [number, number][] = [];
    const canvas = stubCanvas();
    const instance = new ScreenRenderer(canvas, {
      onSize: (width, height) => sizes.push([width, height]),
    });
    instance.configure({ codec: "avc1.640028" });

    instance.decodeChunk(au(AU_KEY));
    expect(sizes.at(-1)).toEqual([1080, 2400]);

    // What iOS sends on rotation: the frames stay portrait, only this changes.
    instance.setRenderRotation(90);
    instance.decodeChunk(au(AU_KEY));
    expect(sizes.at(-1)).toEqual([2400, 1080]);
    expect([canvas.width, canvas.height]).toEqual([2400, 1080]);

    // Back to upright, and the canvas follows.
    instance.setRenderRotation(0);
    instance.decodeChunk(au(AU_KEY));
    expect(sizes.at(-1)).toEqual([1080, 2400]);
  });

  test("support probing is what drives the fallback", async () => {
    expect(await isStreamSupported({ codec: "avc1.640028" })).toBe(true);
    expect(await isStreamSupported({ codec: "unsupported.codec" })).toBe(false);
  });
});
