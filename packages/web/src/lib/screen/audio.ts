/**
 * Low-latency browser playback for the provider's encoded device audio.
 *
 * The provider sends raw Opus packets. WebCodecs decodes them and Web Audio
 * supplies the only volume control in this file, so no UI action here can
 * change the physical device's volume or routing.
 */

import { AUDIO_PACKET, type ServerMessage } from "@yard/protocol";

export type AudioCodecInfo = Extract<ServerMessage, { type: "audio.codec" }>;

const HEADER_BYTES = 9;
const START_BUFFER_SECONDS = 0.04;
const MAX_BUFFER_SECONDS = 0.25;
const MAX_DECODE_QUEUE = 8;

function configFor(info: AudioCodecInfo): AudioDecoderConfig {
  return {
    codec: info.codec,
    sampleRate: info.sampleRate,
    numberOfChannels: info.channels,
  };
}

export async function isAudioStreamSupported(info: AudioCodecInfo): Promise<boolean> {
  if (typeof AudioDecoder === "undefined" || typeof AudioContext === "undefined") return false;
  const { supported } = await AudioDecoder.isConfigSupported(configFor(info));
  return supported === true;
}

export class BrowserAudio {
  private readonly context: AudioContext;
  private readonly gain: GainNode;
  private decoder: AudioDecoder | null = null;
  private firstTimestamp: bigint | null = null;
  private nextStart = 0;

  constructor(volume: number) {
    this.context = new AudioContext({ latencyHint: "interactive" });
    this.gain = this.context.createGain();
    this.gain.gain.value = volume;
    this.gain.connect(this.context.destination);
  }

  async configure(info: AudioCodecInfo): Promise<boolean> {
    if (!(await isAudioStreamSupported(info))) return false;

    closeQuietly(this.decoder);
    this.firstTimestamp = null;
    this.nextStart = 0;
    this.decoder = new AudioDecoder({
      output: (data) => this.play(data),
      error: (error) => console.warn("[audio]", error),
    });
    this.decoder.configure(configFor(info));
    return true;
  }

  setVolume(volume: number) {
    this.gain.gain.value = volume;
  }

  /** Must be called from a user gesture to satisfy browser autoplay policy. */
  activate() {
    if (this.context.state === "suspended") void this.context.resume();
  }

  decodeChunk(frame: Uint8Array) {
    const decoder = this.decoder;
    if (
      decoder?.state !== "configured" ||
      frame.length <= HEADER_BYTES ||
      frame[0] !== AUDIO_PACKET ||
      decoder.decodeQueueSize > MAX_DECODE_QUEUE
    ) {
      return;
    }

    const timestamp = new DataView(frame.buffer, frame.byteOffset + 1, 8).getBigUint64(0);
    this.firstTimestamp ??= timestamp;
    decoder.decode(
      new EncodedAudioChunk({
        type: "key",
        timestamp: Number(timestamp - this.firstTimestamp),
        data: frame.subarray(HEADER_BYTES),
      }),
    );
  }

  reset() {
    closeQuietly(this.decoder);
    this.decoder = null;
    this.firstTimestamp = null;
    this.nextStart = 0;
  }

  destroy() {
    this.reset();
    this.gain.disconnect();
    void this.context.close();
  }

  private play(data: AudioData) {
    try {
      // Do not build a hidden queue while autoplay is blocked. Once the user
      // interacts, the next packet starts a fresh low-latency schedule.
      if (this.context.state !== "running") return;

      const buffer = this.context.createBuffer(
        data.numberOfChannels,
        data.numberOfFrames,
        data.sampleRate,
      );
      for (let channel = 0; channel < data.numberOfChannels; channel++) {
        data.copyTo(buffer.getChannelData(channel), {
          planeIndex: channel,
          format: "f32-planar",
        });
      }

      const now = this.context.currentTime;
      if (this.nextStart < now || this.nextStart > now + MAX_BUFFER_SECONDS) {
        this.nextStart = now + START_BUFFER_SECONDS;
      }

      const source = this.context.createBufferSource();
      source.buffer = buffer;
      source.connect(this.gain);
      source.start(this.nextStart);
      this.nextStart += buffer.duration;
    } finally {
      data.close();
    }
  }
}

function closeQuietly(decoder: AudioDecoder | null) {
  if (decoder?.state !== "closed") decoder?.close();
}
