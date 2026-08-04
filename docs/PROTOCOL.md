# Protocol — phase 2

> **Status: not yet built.** Design contract; update as the code lands.

`packages/protocol` is the **single source of truth** for every message on the
control and session planes: zod schemas in TypeScript, with a codegen step that
emits the Rust mirror.

```
packages/protocol/
├── src/
│   ├── control.ts     provider ↔ coordinator envelopes
│   ├── session.ts     browser ↔ provider envelopes
│   └── index.ts
├── scripts/gen-rust.ts
└── test/fake-provider.ts
```

```bash
bun run protocol:gen   # → packages/provider/crates/farm-protocol/src/generated.rs
```

The generated file is serde structs plus a tagged enum. **It is committed**, and
CI runs:

```bash
bun run protocol:gen && git diff --exit-code
```

so a schema change that wasn't regenerated fails the build. This makes
impossible the exact failure mode `stf-ios-provider`'s README warns about: a
stale protobuf copy that encodes cleanly and delivers nothing.

## Encoding

**JSON for control messages.** They are low-rate and being able to read them in
a log or a browser devtools frame inspector is worth more than the bytes.

**Binary framing for video access units only** — `[type byte][AU]`, where the
type byte is `0 = key`, `1 = delta`, `2 = key-with-reset`. This is the framing
`stf-ios-provider/src/frontend/hevc-screen.js` already speaks, documented in
that directory's `INTEGRATION.md`.

## Control plane (provider → coordinator, outbound WSS)

`/api/providers/connect`, bearer-authenticated with a `providerToken`.

State machine: `hello` → reconcile that provider's device rows → heartbeat loop
→ event ingest → command dispatch with per-command timeouts.

Planned message families:

| Direction | Messages |
|---|---|
| provider → coordinator | `hello` (id, version, publicBaseUrl, hostname), `heartbeat`, `device.present`, `device.absent`, `device.state`, `device.display`, `device.battery`, `command.result`, `install.progress` |
| coordinator → provider | `session.authorize` (deviceId, reservationId), `session.revoke`, `device.reboot`, `device.rotate`, `device.launch`, `device.uninstall`, `device.apps`, `device.adb.expose` |

A provider's devices flip to `absent` when its socket drops — that is the
coordinator's job, not a timeout on the provider side.

## Session plane (browser → provider, direct WSS)

`wss://<publicBaseUrl>/s/<deviceId>?token=<jwt>`

The provider verifies the coordinator-signed Ed25519 JWT locally against the
JWKS it cached at startup, and checks the token's `reservationId` against the
one the coordinator last authorized for that device.

| Direction | Messages |
|---|---|
| provider → browser | `{type:"codec"}` handshake (codec string + description), binary AU frames, `clipboard`, `install.progress`, `error` |
| browser → provider | `touch` (down/move/up), `key`, `text`, `clipboard.get`/`set`, `rotate`, `keyframe` request |

## Artifact plane (browser → provider, HTTPS)

Same port, same token.

- `POST /s/:deviceId/install` — streams the APK/IPA to the scratch dir,
  installs, deletes, reports progress over the session WebSocket
- `GET /s/:deviceId/screenshot.png`
- `GET /s/:deviceId/mjpeg` — `multipart/x-mixed-replace` fallback, ~3 fps cap

## Fake provider

`packages/protocol/test/fake-provider.ts` speaks the control plane and registers
synthetic devices. It lets the coordinator and the whole web UI be developed and
regression-tested **with no hardware attached**, and is the thing to validate the
gateway against before any Rust is written.
