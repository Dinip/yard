# Architecture

## The one idea

**The coordinator owns identity, inventory, reservations and policy. Providers
own devices and every byte of high-bandwidth traffic.** Video and input go
browser↔provider directly; the coordinator is never on the data path.

This is the single structural difference from STF, which proxied every device's
MJPEG stream through the app server (`stf/lib/units/poorxy`) and so made the main
instance the bottleneck for exactly the traffic that scales with device count.

## Four planes

```
                    ┌──────────────── coordinator ────────────────┐
  browser ───1───►  │ Hono · tRPC · better-auth · Drizzle/Postgres │
     │              │ provider gateway (WSS) · session-token signer│
     │              └─────────────────────┬───────────────────────┘
     │                                    │ 2. control plane
     │                                    │    (provider dials out, WSS)
     │              ┌─────────────────────┴───────────────────────┐
     │  3. session  │              provider                        │
     ├──── WSS ────►│   video access units + input events          │
     │              │                                              │
     │  4. artifact │   ┌──────────────────┬───────────────────┐   │
     └─── HTTPS ───►│   │ backend-ios      │ backend-android   │   │
       upload ipa/  │   │ CoreDevice / RSD │ adb + scrcpy      │   │
       apk, fetch   │   │ HEVC             │ H.264             │   │
       screenshot   │   └──────────────────┴───────────────────┘   │
                    └──────────────────────────────────────────────┘
```

### 1. App plane — browser ↔ coordinator

tRPC over HTTP with a better-auth session cookie. Device list, reservations,
users, admin. Small payloads only. Never carries a video frame or an APK.

### 2. Control plane — provider → coordinator

One persistent **outbound** WSS per provider. Registration, heartbeat, device
inventory and state, and coordinator-issued commands (reboot, expose adb,
launch/uninstall an app, authorize/revoke a session).

Outbound-only is the load-bearing choice: providers need no inbound reachability
from the coordinator, so they work identically on-box, in another datacentre, or
behind a NAT in an office.

### 3. Session plane — browser ↔ provider

Direct WSS at `wss://<provider publicBaseUrl>/s/<deviceId>?token=<jwt>`. Carries
video access units and input events. The provider verifies the
coordinator-signed token locally against a cached JWKS.

**Input never round-trips through the coordinator**, so interaction latency is
browser↔provider RTT and nothing else.

### 4. Artifact plane — browser ↔ provider

Plain HTTPS on the same provider port, authenticated by the same session token.

- `POST /s/:deviceId/install` streams the APK/IPA to the provider's scratch dir,
  installs it, deletes it, and reports progress over the session WebSocket.
- `GET /s/:deviceId/screenshot.png` returns the image directly.

Nothing is stored server-side. There is no object storage anywhere in the
system, and no `app`/artifact table in the database — see
[DATA-MODEL.md](./DATA-MODEL.md).

### Not a plane: the metrics listener

A provider can also expose device CPU, memory and temperature for Prometheus, on
a **second port of its own** (`metrics.bind`, default 9100). That is deliberately
not counted as a fifth plane: every plane above carries user traffic and is
authenticated end to end, and this carries neither. It is an operator side door,
and calling it a plane would make "plane" mean "socket".

It is separate from the session port precisely *because* it is not a plane. That
port is browser-facing, carries a CORS layer and session tokens, and is publicly
TLS-terminated; a scraper has none of those, and putting `/metrics` there would
hand it the CORS layer. There is no auth on the metrics port, so the operator is
expected to bind it where only their monitoring can reach it. See
[PROVIDER.md](./PROVIDER.md#metrics).

## Session tokens

Ed25519 JWTs carrying `deviceId`, `userId`, `reservationId`, `exp ≈ 60s`,
refreshed by the client while the tab is open.

The coordinator publishes a JWKS at `/.well-known/yard-jwks.json`; each provider
fetches it at startup and caches it. Consequences worth stating:

- No shared secret is distributed to providers.
- A provider keeps serving an already-authorized session if the coordinator
  restarts or goes away entirely.
- Revocation is a *push* over the control plane (`session.revoke`), not a
  token-lifetime side effect — the short `exp` is a backstop, not the mechanism.

## Why TLS is not cosmetic

`WebCodecs` requires a secure context from any non-loopback origin. **HTTPS is a
hard requirement for streaming**, not a production nicety. Caddy terminates TLS
for the coordinator and the SPA; on multi-machine deploys each provider host runs
its own terminator and advertises its own `publicBaseUrl`, which the coordinator
merely hands to the browser.

## Failure semantics

| Failure | Behaviour |
|---|---|
| Coordinator dies mid-session | Streaming continues (provider holds the authorized reservation). Device list goes stale. Reconnect reconciles. |
| Provider socket drops | Its devices flip to `absent`; its reservations release. |
| Device unplugged mid-session | Backend reports unhealthy; the UI shows it rather than a frozen frame. |
| Two users reserve at once | Postgres partial unique index picks one winner; the loser gets `CONFLICT`. |
| Cleanup fails, hangs, or its provider dies | The device lands on `ready` regardless — under the provider's own deadline, or the coordinator's sweep if the provider is gone. Cleanup can never strand a device. |

## Reservations replace STF's group model

STF made the **device** the arbiter of ownership
(`stf/lib/units/ios-device/plugins/group.js`, ported into
`stf-ios-provider/src/group.rs`), which is why upstream needed idle-group expiry
timers on every device.

Here the **database** is the arbiter. The provider holds only the currently
authorized `reservationId` per device, pushed by the coordinator, and validates
the JWT in each session connect against it. No arbitration, no timers on-device.

## Related documents

- [DATA-MODEL.md](./DATA-MODEL.md) — tables and why each exists
- [COORDINATOR.md](./COORDINATOR.md) — the backend package
- [WEB.md](./WEB.md) — the SPA
- [PROVIDER.md](./PROVIDER.md) — the Rust provider (phases 3–4)
- [PROTOCOL.md](./PROTOCOL.md) — wire contract (phase 2)
- [CLEANUP.md](./CLEANUP.md) — resetting a device between users
- [REFERENCES.md](./REFERENCES.md) — what to read in the STF / stf-ios-provider sources
