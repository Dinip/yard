/**
 * Session plane client: browser ↔ provider, direct.
 *
 * The coordinator issues the token and is then out of the way — every byte of
 * video, input, screenshot and APK/IPA on this path goes to the provider's own
 * origin. Nothing here may ever be routed back through the coordinator.
 *
 * Tokens live ~60s and are only checked when a request starts, so the socket
 * outlives its token by design; revocation arrives as a `session.closed` push,
 * not as an expiry. Each artifact-plane request mints a fresh one.
 */

import type { ClientMessage, FileListing, ServerMessage } from "@yard/protocol";
import { trpcClient } from "@/lib/trpc";

export type SessionState = "idle" | "connecting" | "open" | "closed";

export interface SessionHandlers {
  onMessage: (message: ServerMessage) => void;
  onBinary: (frame: Uint8Array) => void;
  onState: (state: SessionState, detail?: string) => void;
}

export interface SessionGrant {
  token: string;
  sessionUrl: string;
  providerBaseUrl: string;
}

export async function mintToken(deviceId: string): Promise<SessionGrant> {
  const grant = await trpcClient.device.sessionToken.mutate({ deviceId });
  return {
    token: grant.token,
    sessionUrl: grant.sessionUrl,
    providerBaseUrl: grant.providerBaseUrl,
  };
}

const RETRY_MIN = 1_000;
const RETRY_MAX = 10_000;

export class DeviceSession {
  private socket: WebSocket | null = null;
  private closedByUs = false;
  /** Set once the coordinator revokes: reconnecting would only 403. */
  private revoked = false;
  private retryDelay = RETRY_MIN;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly deviceId: string,
    private readonly handlers: SessionHandlers,
  ) {}

  /**
   * Stop reconnecting. The reservation is gone, so every retry would mint a
   * token the provider refuses — and the user needs to see why, not a spinner.
   */
  markRevoked() {
    this.revoked = true;
  }

  async open() {
    this.closedByUs = false;
    this.handlers.onState("connecting");
    let grant: SessionGrant;
    try {
      grant = await mintToken(this.deviceId);
    } catch (error) {
      this.handlers.onState("closed", error instanceof Error ? error.message : "no session token");
      this.scheduleRetry();
      return;
    }
    if (this.closedByUs) return;

    const socket = new WebSocket(`${grant.sessionUrl}?token=${encodeURIComponent(grant.token)}`);
    // Blobs cost an async arrayBuffer() round-trip and a garbage allocation per
    // access unit; the decoder wants bytes now.
    socket.binaryType = "arraybuffer";
    this.socket = socket;

    socket.onopen = () => {
      this.retryDelay = RETRY_MIN;
      this.handlers.onState("open");
    };
    socket.onmessage = (event) => {
      if (typeof event.data === "string") {
        try {
          this.handlers.onMessage(JSON.parse(event.data) as ServerMessage);
        } catch {
          console.warn("[session] unparseable text frame");
        }
        return;
      }
      this.handlers.onBinary(new Uint8Array(event.data as ArrayBuffer));
    };
    socket.onerror = () => {
      // The close event carries the useful detail; this only marks that the
      // failure was transport-level rather than a clean provider close.
      console.warn("[session] socket error");
    };
    socket.onclose = (event) => {
      this.socket = null;
      if (this.closedByUs) return;
      this.handlers.onState("closed", event.reason || undefined);
      this.scheduleRetry();
    };
  }

  /**
   * A provider restart or a blip should not cost the user their session: the
   * reservation is still theirs, so reconnecting is just minting a new token.
   * Backs off like the provider's own control-plane client does.
   */
  private scheduleRetry() {
    if (this.closedByUs || this.revoked || this.retryTimer) return;
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      if (this.closedByUs || this.revoked) return;
      void this.open();
    }, this.retryDelay);
    this.retryDelay = Math.min(this.retryDelay * 2, RETRY_MAX);
  }

  send(message: ClientMessage) {
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.socket.send(JSON.stringify(message));
    }
  }

  close() {
    this.closedByUs = true;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.socket?.close();
    this.socket = null;
    this.handlers.onState("idle");
  }
}

/**
 * `GET /s/:deviceId/mjpeg` — the degraded fallback stream.
 *
 * A `multipart/x-mixed-replace` URL for an `<img>`, used only when the browser
 * cannot decode the real stream. The token rides in the query string because an
 * `<img>` cannot carry a header; the provider re-checks the reservation on every
 * frame, so the URL outliving its token grants nothing.
 */
export async function fallbackStreamUrl(deviceId: string): Promise<string> {
  const grant = await mintToken(deviceId);
  return `${grant.providerBaseUrl}/s/${deviceId}/mjpeg?token=${encodeURIComponent(grant.token)}`;
}

/** `GET /s/:deviceId/screenshot.png` — a PNG straight from the device. */
export async function fetchScreenshot(deviceId: string): Promise<Blob> {
  const grant = await mintToken(deviceId);
  const response = await fetch(
    `${grant.providerBaseUrl}/s/${deviceId}/screenshot.png?token=${encodeURIComponent(grant.token)}`,
  );
  if (!response.ok) throw new Error(await response.text());
  return response.blob();
}

/**
 * `GET /s/:deviceId/files` — one directory on the device.
 *
 * `path` absent opens wherever the backend starts, so the browser never has to
 * know that Android means `/sdcard` and iOS means the AFC media root.
 */
export async function listDeviceFiles(deviceId: string, path?: string): Promise<FileListing> {
  const grant = await mintToken(deviceId);
  const url = new URL(`${grant.providerBaseUrl}/s/${deviceId}/files`);
  url.searchParams.set("token", grant.token);
  if (path) url.searchParams.set("path", path);

  const response = await fetch(url);
  if (!response.ok)
    throw new Error((await response.text()) || `listing failed (${response.status})`);
  return (await response.json()) as FileListing;
}

/**
 * `GET /s/:deviceId/file` — the bytes of one file, straight from the provider.
 *
 * The provider stages it off the device, serves it, and deletes it; the only
 * lasting trace is the coordinator's `device.file_pull` audit row. Nothing is
 * stored, here or there.
 */
export async function fetchDeviceFile(deviceId: string, path: string): Promise<Blob> {
  const grant = await mintToken(deviceId);
  const url = new URL(`${grant.providerBaseUrl}/s/${deviceId}/file`);
  url.searchParams.set("token", grant.token);
  url.searchParams.set("path", path);

  const response = await fetch(url);
  if (!response.ok)
    throw new Error((await response.text()) || `download failed (${response.status})`);
  return response.blob();
}

export interface InstallResult {
  ok: boolean;
  filename?: string;
  size?: number;
  sha256?: string;
  error?: string;
}

export interface PreloadGrant {
  deviceId: string;
  providerBaseUrl: string;
  token: string;
}

function uploadApp(
  url: string,
  file: File,
  onProgress: (fraction: number) => void,
): Promise<InstallResult> {
  return new Promise<InstallResult>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", url);
    xhr.setRequestHeader("x-yard-filename", file.name);
    xhr.setRequestHeader("content-type", "application/octet-stream");
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) onProgress(event.loaded / event.total);
    };
    xhr.onload = () => {
      let body: InstallResult;
      try {
        body = JSON.parse(xhr.responseText) as InstallResult;
      } catch {
        reject(new Error(xhr.responseText || `install failed (${xhr.status})`));
        return;
      }
      // A failed install answers with a JSON body — the error belongs to the
      // device, not the transport, so surface its message.
      if (xhr.status >= 200 && xhr.status < 300 && body.ok) resolve(body);
      else reject(new Error(body.error ?? `install failed (${xhr.status})`));
    };
    xhr.onerror = () => reject(new Error("upload failed"));
    xhr.send(file);
  });
}

/**
 * `POST /s/:deviceId/install` — streams the build to the provider, which
 * installs it and deletes it. Admin preloads use a separate protected endpoint;
 * this session path remains transient and is represented by the coordinator's
 * audit-log row.
 *
 * XHR rather than fetch because upload progress is the whole point and
 * `fetch` still has no portable way to report it.
 */
export function installApp(
  deviceId: string,
  file: File,
  onProgress: (fraction: number) => void,
): Promise<InstallResult> {
  return mintToken(deviceId).then((grant) =>
    uploadApp(
      `${grant.providerBaseUrl}/s/${deviceId}/install?token=${encodeURIComponent(grant.token)}`,
      file,
      onProgress,
    ),
  );
}

/**
 * `POST /s/:deviceId/preload` — streams an admin-selected app to one idle
 * device. The grant is scoped to this endpoint and platform by the coordinator.
 */
export function preloadApp(
  grant: PreloadGrant,
  file: File,
  onProgress: (fraction: number) => void,
): Promise<InstallResult> {
  return uploadApp(
    `${grant.providerBaseUrl}/s/${grant.deviceId}/preload?token=${encodeURIComponent(grant.token)}`,
    file,
    onProgress,
  );
}
