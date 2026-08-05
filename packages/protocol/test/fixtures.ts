/**
 * Canonical wire messages, parsed by **both** sides.
 *
 * `bun test packages/protocol` asserts zod accepts every one of these and that
 * re-encoding is byte-stable. `cargo test -p farm-protocol` reads the same file
 * and asserts serde does too.
 *
 * A schema change that breaks one language but not the other fails here, which
 * is the whole reason the file is shared rather than duplicated.
 */
export const controlFixtures = {
  hello: {
    type: "hello",
    protocolVersion: 1,
    providerId: "provider-lab-1",
    name: "lab-1",
    version: "0.1.0",
    publicBaseUrl: "https://lab-1.example.com",
    hostname: "lab-1.local",
    devices: [
      {
        id: "00008120-000C1D2E3F401234",
        platform: "ios",
        status: "ready",
        name: "iPhone 15 Pro",
        model: "iPhone16,1",
        osVersion: "17.4.1",
        display: { width: 1179, height: 2556, scale: 3, rotation: 0 },
        battery: { level: 0.82, state: "discharging" },
        streamCodec: "hev1.1.6.L93.B0",
      },
      {
        id: "R5CT10ABCDE",
        platform: "android",
        status: "present",
        manufacturer: "Google",
        model: "Pixel 8",
        osVersion: "14",
        abi: "arm64-v8a",
        sdk: 34,
        streamCodec: "avc1.640028",
      },
    ],
  },
  heartbeat: { type: "heartbeat", at: 1770000000000 },
  deviceStatus: { type: "device.status", deviceId: "R5CT10ABCDE", status: "ready" },
  deviceStatusWithNote: {
    type: "device.status",
    deviceId: "R5CT10ABCDE",
    status: "unhealthy",
    note: "adb transport dropped",
  },
  commandResult: {
    type: "command.result",
    id: "cmd-1",
    ok: true,
    data: { apps: [{ id: "com.example.app", name: "Example", version: "1.2.3" }] },
  },
  commandResultError: { type: "command.result", id: "cmd-2", ok: false, error: "device is busy" },
  installFinished: {
    type: "install.finished",
    deviceId: "R5CT10ABCDE",
    userId: "user-1",
    filename: "app-release.apk",
    size: 48293012,
    sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ok: true,
  },
} as const;

export const coordinatorFixtures = {
  helloAck: {
    type: "hello.ack",
    protocolVersion: 1,
    heartbeatIntervalMs: 15000,
    jwksUrl: "https://farm.example.com/.well-known/farm-jwks.json",
    issuer: "https://farm.example.com",
    webOrigins: ["https://farm.example.com"],
  },
  helloReject: { type: "hello.reject", reason: "unknown provider token" },
  authorize: {
    type: "command",
    id: "cmd-1",
    payload: {
      kind: "session.authorize",
      deviceId: "R5CT10ABCDE",
      reservationId: "res-1",
      userId: "user-1",
    },
  },
  revoke: {
    type: "command",
    id: "cmd-2",
    payload: { kind: "session.revoke", deviceId: "R5CT10ABCDE", reason: "reservation expired" },
  },
  adbExpose: {
    type: "command",
    id: "cmd-3",
    payload: { kind: "device.adb.expose", deviceId: "R5CT10ABCDE" },
  },
} as const;

export const clientFixtures = {
  down: { type: "pointer.down", pointerId: 0, at: { x: 0.5, y: 0.25 } },
  move: { type: "pointer.move", pointerId: 0, at: { x: 0.5, y: 0.75 } },
  up: { type: "pointer.up", pointerId: 0, at: { x: 0.5, y: 0.75 } },
  key: { type: "key", key: "Enter", down: true },
  text: { type: "text", text: "hello world" },
  clipboardSet: { type: "clipboard.set", text: "copied" },
  clipboardGet: { type: "clipboard.get" },
  keyframe: { type: "keyframe" },
  rotate: { type: "rotate", degrees: 90 },
} as const;

export const serverFixtures = {
  codec: {
    type: "codec",
    codec: "hev1.1.6.L93.B0",
    description: "AQFAAAAAAAAAAAAAAAAAAAAA",
    display: { width: 1179, height: 2556, scale: 3 },
  },
  clipboard: { type: "clipboard", text: "copied" },
  clipboardEmpty: { type: "clipboard", text: null },
  installProgress: { type: "install.progress", progress: 0.42, stage: "uploading" },
  installResult: { type: "install.result", ok: true, appId: "com.example.app" },
  sessionClosed: { type: "session.closed", reason: "reservation released" },
} as const;
