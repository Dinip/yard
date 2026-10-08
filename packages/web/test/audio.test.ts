import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { AUDIO_PACKET } from "@yard/protocol";

const decoders: StubAudioDecoder[] = [];
const contexts: StubAudioContext[] = [];

class StubAudioDecoder {
  static isConfigSupported(config: AudioDecoderConfig) {
    return Promise.resolve({ supported: config.codec === "opus", config });
  }

  state: CodecState = "unconfigured";
  decodeQueueSize = 0;
  config: AudioDecoderConfig | null = null;
  chunks: EncodedAudioChunk[] = [];

  constructor(readonly init: AudioDecoderInit) {
    decoders.push(this);
  }

  configure(config: AudioDecoderConfig) {
    this.config = config;
    this.state = "configured";
  }

  decode(chunk: EncodedAudioChunk) {
    this.chunks.push(chunk);
  }

  close() {
    this.state = "closed";
  }
}

class StubAudioContext {
  state: AudioContextState = "running";
  currentTime = 0;
  destination = {} as AudioDestinationNode;
  gain = {
    gain: { value: 0 },
    connect() {},
    disconnect() {},
  } as unknown as GainNode;

  starts: number[] = [];

  constructor(readonly options: AudioContextOptions) {
    contexts.push(this);
  }

  createGain() {
    return this.gain;
  }

  createBuffer(channels: number, frames: number, sampleRate: number) {
    return {
      duration: frames / sampleRate,
      getChannelData: () => new Float32Array(frames),
      numberOfChannels: channels,
    };
  }

  createBufferSource() {
    return { buffer: null, connect() {}, start: (at: number) => this.starts.push(at) };
  }

  resume() {
    this.state = "running";
    return Promise.resolve();
  }

  close() {
    this.state = "closed";
    return Promise.resolve();
  }
}

class StubEncodedAudioChunk {
  readonly timestamp: number;
  readonly data: Uint8Array;

  constructor(init: EncodedAudioChunkInit) {
    this.timestamp = init.timestamp;
    this.data = new Uint8Array(init.data as ArrayBuffer);
  }
}

function packet(timestamp: bigint, payload: number[]) {
  const frame = new Uint8Array(9 + payload.length);
  frame[0] = AUDIO_PACKET;
  new DataView(frame.buffer).setBigUint64(1, timestamp);
  frame.set(payload, 9);
  return frame;
}

let BrowserAudio: typeof import("../src/lib/screen/audio.ts").BrowserAudio;

beforeEach(async () => {
  decoders.length = 0;
  contexts.length = 0;
  Object.assign(globalThis, {
    AudioDecoder: StubAudioDecoder,
    AudioContext: StubAudioContext,
    EncodedAudioChunk: StubEncodedAudioChunk,
  });
  ({ BrowserAudio } = await import("../src/lib/screen/audio.ts"));
});

afterEach(() => {
  for (const key of ["AudioDecoder", "AudioContext", "EncodedAudioChunk"]) {
    delete (globalThis as Record<string, unknown>)[key];
  }
});

describe("BrowserAudio", () => {
  test("uses the source sample rate and keeps playback contiguous across packet jitter", async () => {
    const audio = new BrowserAudio(1);
    await audio.configure({ type: "audio.codec", codec: "opus", sampleRate: 48_000, channels: 2 });
    const context = contexts[0]!;
    expect(context.options.sampleRate).toBe(48_000);
    let closed = 0;
    const output = () =>
      decoders[0]!.init.output({
        numberOfChannels: 2,
        numberOfFrames: 960,
        sampleRate: 48_000,
        copyTo() {},
        close() {
          closed++;
        },
      } as unknown as AudioData);
    output();
    context.currentTime = 0.03;
    output();
    context.currentTime = 0.075;
    output();
    expect(context.starts[0]).toBeCloseTo(0.08);
    expect(context.starts[1]).toBeCloseTo(0.1);
    expect(context.starts[2]).toBeCloseTo(0.12);
    expect(closed).toBe(3);
    audio.destroy();
  });

  test("decodes raw Opus with source-relative timestamps and browser-only gain", async () => {
    const audio = new BrowserAudio(0.75);
    expect(contexts[0]?.gain.gain.value).toBe(0.75);
    expect(
      await audio.configure({
        type: "audio.codec",
        codec: "opus",
        sampleRate: 48_000,
        channels: 2,
      }),
    ).toBe(true);

    audio.decodeChunk(packet(8_000_000n, [0xf8, 0xff, 0xfe]));
    audio.decodeChunk(packet(8_020_000n, [0xf8, 0xff, 0xfe]));

    expect(decoders[0]?.config).toEqual({
      codec: "opus",
      sampleRate: 48_000,
      numberOfChannels: 2,
    });
    expect(decoders[0]?.chunks.map((chunk) => chunk.timestamp)).toEqual([0, 20_000]);

    audio.setVolume(0.2);
    expect(contexts[0]?.gain.gain.value).toBe(0.2);
    audio.destroy();
  });
});
