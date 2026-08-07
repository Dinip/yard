/**
 * Records what the viewer is watching.
 *
 * Deliberately the whole feature: there is no provider code, no wire message
 * and no coordinator involvement behind this. The frames are already decoded
 * and already painted, so recording them is the browser recording its own
 * canvas — the same thing the user could do with any screen recorder, which is
 * also why there is nothing here for the audit log to say. The coordinator
 * cannot observe it, and a row claiming otherwise would be a fiction.
 */

/**
 * The hard cap. A recording nobody stops is how a tab ends up holding a
 * gigabyte of chunks, so the timer finalises and *saves* rather than
 * discarding — an accidental two minutes is still evidence.
 */
export const MAX_RECORDING_MS = 120_000;

/**
 * MP4 first, because it is the file people can actually open and send on.
 *
 * `MediaRecorder` only grew MP4 output recently and Firefox still has WebM
 * only, so this is a preference rather than an assumption: the first supported
 * type wins and the extension follows from what was chosen, so the download is
 * never named `.mp4` by a recorder that produced WebM.
 */
const PREFERRED_TYPES = [
  "video/mp4;codecs=avc1.42E01E",
  "video/mp4",
  "video/webm;codecs=vp9",
  "video/webm",
];

function pickMimeType(): string | undefined {
  if (typeof MediaRecorder === "undefined") return undefined;
  return PREFERRED_TYPES.find((type) => MediaRecorder.isTypeSupported(type));
}

/** Whether this browser can record at all — the button is hidden if not. */
export function isRecordingSupported(): boolean {
  return (
    typeof MediaRecorder !== "undefined" &&
    typeof HTMLCanvasElement.prototype.captureStream === "function" &&
    pickMimeType() !== undefined
  );
}

export function extensionFor(mimeType: string): string {
  return mimeType.startsWith("video/mp4") ? "mp4" : "webm";
}

/**
 * Ceiling on frames pushed into the recording.
 *
 * The mirror is redrawn on every animation frame, which is up to the display's
 * refresh rate; 30 is plenty for a screen recording and halves the file.
 */
const CAPTURE_FPS = 30;
const MIN_FRAME_GAP = 1000 / CAPTURE_FPS;

export interface Recording {
  blob: Blob;
  mimeType: string;
  /** True when the 2-minute cap ended it rather than the user. */
  cappedOut: boolean;
}

/**
 * A recording in progress.
 *
 * **It captures a fixed-size mirror of the live canvas, not the canvas
 * itself.** `ScreenRenderer` reassigns `canvas.width`/`height` whenever the
 * device's geometry changes, and a mid-stream resize is not something
 * `MediaRecorder` handles portably — an MP4 track has one geometry for its
 * whole length. So the size is fixed at record-start and each frame is drawn
 * into it aspect-fitted: rotating the device mid-recording letterboxes, which
 * is what someone recording a rotation bug needs, instead of ending the file.
 *
 * Mirroring also keeps this off the live context, which is created
 * `desynchronized` for latency and is not the thing to hang a capture on.
 *
 * **Frames are pushed explicitly, with `captureStream(0)` +
 * `requestFrame()`.** A capture rate cannot be used here: the mirror is not in
 * the document, so nothing composites it, and Chrome emits a frame from an
 * uncomposited canvas only occasionally whatever the canvas is told. Measured
 * on a real device, `captureStream(30)` produced **three frames in
 * forty-one seconds** of continuous motion — a correctly timed slideshow, which
 * is a worse failure than an error because the file looks fine until it is
 * played. `requestFrame` is the API for driving exactly this case.
 */
export class ScreenRecorder {
  private readonly mirror: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly recorder: MediaRecorder;
  private readonly track: CanvasCaptureMediaStreamTrack;
  private readonly chunks: Blob[] = [];
  private frameHandle: number | null = null;
  private lastFrameAt = 0;
  private capTimer: ReturnType<typeof setTimeout> | null = null;
  private cappedOut = false;
  private settle: ((recording: Recording) => void) | null = null;
  private settled: Promise<Recording> | null = null;

  readonly startedAt = Date.now();
  readonly mimeType: string;

  private constructor(
    private readonly source: HTMLCanvasElement,
    mimeType: string,
  ) {
    this.mimeType = mimeType;

    this.mirror = document.createElement("canvas");
    // Whatever the canvas is showing right now. A canvas that has not painted
    // yet has no useful size, which `start` refuses rather than recording a
    // 300×150 default.
    this.mirror.width = source.width;
    this.mirror.height = source.height;

    const ctx = this.mirror.getContext("2d", { alpha: false });
    if (!ctx) throw new Error("this browser would not give a 2d context");
    this.ctx = ctx;

    // 0 means "only the frames I ask for", which is the whole point — see the
    // class comment.
    const stream = this.mirror.captureStream(0);
    this.track = stream.getVideoTracks()[0] as CanvasCaptureMediaStreamTrack;

    this.recorder = new MediaRecorder(stream, { mimeType });
    this.recorder.ondataavailable = (event) => {
      if (event.data.size > 0) this.chunks.push(event.data);
    };
  }

  /** Null when the canvas has not painted yet, or the browser cannot record. */
  static start(source: HTMLCanvasElement): ScreenRecorder | null {
    const mimeType = pickMimeType();
    if (!mimeType || !source.width || !source.height) return null;

    const recorder = new ScreenRecorder(source, mimeType);
    recorder.recorder.start();
    recorder.pump();
    recorder.capTimer = setTimeout(() => {
      recorder.cappedOut = true;
      void recorder.stop();
    }, MAX_RECORDING_MS);
    return recorder;
  }

  /** Milliseconds since the recording began. */
  elapsed(): number {
    return Date.now() - this.startedAt;
  }

  /**
   * Finalises and answers the file. Safe to call twice — the second caller
   * waits on the same result, which matters because the cap timer and the stop
   * button race whenever a user clicks stop at 1:59.
   */
  stop(): Promise<Recording> {
    if (this.settled) return this.settled;

    this.settled = new Promise<Recording>((resolve) => {
      this.settle = resolve;
    });

    if (this.frameHandle !== null) cancelAnimationFrame(this.frameHandle);
    this.frameHandle = null;
    if (this.capTimer) clearTimeout(this.capTimer);
    this.capTimer = null;

    const finish = () =>
      this.settle?.({
        blob: new Blob(this.chunks, { type: this.mimeType }),
        mimeType: this.mimeType,
        cappedOut: this.cappedOut,
      });

    this.recorder.onstop = finish;
    if (this.recorder.state === "inactive") finish();
    else this.recorder.stop();

    return this.settled;
  }

  /**
   * Copies the live canvas into the mirror once per animation frame.
   *
   * Aspect-fitted onto a cleared background, so a geometry change part way
   * through appears as a letterboxed picture rather than a stretched one. This
   * runs only while recording.
   */
  private pump = () => {
    this.frameHandle = requestAnimationFrame(this.pump);

    const now = performance.now();
    if (now - this.lastFrameAt < MIN_FRAME_GAP) return;
    this.lastFrameAt = now;

    const { width: sw, height: sh } = this.source;
    if (sw <= 0 || sh <= 0) return;

    const { width: dw, height: dh } = this.mirror;
    const scale = Math.min(dw / sw, dh / sh);
    const w = sw * scale;
    const h = sh * scale;

    this.ctx.fillStyle = "#000";
    this.ctx.fillRect(0, 0, dw, dh);
    this.ctx.drawImage(this.source, (dw - w) / 2, (dh - h) / 2, w, h);

    // Drawing is not enough on its own; see the class comment.
    this.track.requestFrame();
  };
}
