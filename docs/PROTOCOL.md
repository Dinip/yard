# Protocol

`packages/protocol` is the **single source of truth** for every message on the
control and session planes: zod schemas in TypeScript, with a codegen step that
emits the Rust mirror.

```
packages/protocol/
├── src/
│   ├── common.ts        DeviceSnapshot, Display, Battery, AppInfo, enums
│   ├── control.ts       provider ↔ coordinator
│   ├── session.ts       browser ↔ provider
│   ├── token.ts         session-token claims, JWKS path
│   ├── registry.ts      named() — gives a schema a Rust type name
│   └── index.ts
├── scripts/gen-rust.ts
└── test/
    ├── fixtures.ts      canonical messages, parsed by both languages
    ├── protocol.test.ts
    └── fake-provider.ts a provider with no hardware
```

## Codegen

```bash
bun run protocol:gen     # → packages/provider/crates/farm-protocol/src/generated.rs
bun run protocol:check   # tests + regen + diff, as CI runs it
```

Only schemas wrapped in `named("Foo", …)` become Rust types. An unregistered
nested object is a **hard error**, not a guess — the generator refuses rather
than inventing a name that would later collide.

The emitter walks zod v4's internals rather than going via JSON Schema, because
JSON Schema erases discriminated-union tags — the entire shape of this protocol.
Output is piped through `rustfmt`, so `cargo fmt --check` and the drift check
cannot demand different bytes from each other.

### The drift guard

`generated.rs` is **committed**, and CI runs `protocol:gen && git diff
--exit-code`. A schema change that wasn't regenerated fails the build. This
makes impossible the exact failure `stf-ios-provider`'s README warns about: a
stale copy of the wire contract that encodes cleanly and delivers nothing.

### Cross-language fixtures

`test/fixtures.ts` holds canonical messages. `bun test packages/protocol`
asserts zod parses them and writes `crates/farm-protocol/tests/fixtures.json`;
`cargo test -p farm-protocol` reads it back and asserts serde re-encodes it
unchanged. A change that breaks one language but not the other fails a test.

This caught a real bug during development: `.optional()` and `.nullable()` were
both mapped to `skip_serializing_if`, so `{"clipboard": {"text": null}}` — which
means *the clipboard is empty* — serialized as an absent field, i.e. *no
clipboard report at all*. They are now distinct:

| zod | Rust | On the wire |
|---|---|---|
| `.optional()` | `Option<T>` + `skip_serializing_if` | key may be absent |
| `.nullable()` | `Option<T>` + `default` | key always present, may be `null` |

## Encoding

**JSON for control messages.** They are low-rate, and reading them in a log is
worth more than the bytes.

**Binary framing for video access units only** — `[type byte][AU]`, where the
type byte is `0 = key`, `1 = delta`, `2 = key-with-reset`. This is the framing
`stf-ios-provider/src/frontend/hevc-screen.js` already speaks; see that
directory's `INTEGRATION.md`.

## Control plane — provider ↔ coordinator ✅ built

`GET /api/providers/connect`, upgraded to WSS, `Authorization: Bearer <pft_…>`.
Authentication happens **before** the upgrade, so a bad credential is a 401
rather than a socket that closes immediately — much easier to read in a provider
log.

State machine: `hello` → reconcile inventory → heartbeat loop → event ingest →
command dispatch.

### provider → coordinator (`ProviderMessage`)

| Message | Effect |
|---|---|
| `hello` | Version + id check, provider row updated, **whole inventory reconciled** |
| `heartbeat` | Refreshes `lastSeenAt` and rearms the timeout |
| `device.upsert` | Full snapshot upserted (hotplug) |
| `device.removed` | Device → `absent` |
| `device.status` / `.display` / `.battery` | Targeted field updates |
| `command.result` | Settles the correlated pending command |
| `install.finished` | Written to `auditLog` — the file is already deleted |

### coordinator → provider (`CoordinatorMessage`)

`hello.ack` (carries the heartbeat interval, the JWKS URL, the token `issuer`,
and `webOrigins` — the browser origins the provider may serve; see below),
`hello.reject`,
`ping`, and `command` — whose `payload` is one of:

`session.authorize` · `session.revoke` · `device.reboot` · `device.rotate` ·
`device.apps` · `device.launch` · `device.uninstall` · `device.adb.expose` ·
`device.adb.unexpose` · `device.restart`

Every command is correlated by id and bounded by a 15s timeout — a provider that
accepts a command and never answers must not wedge the caller.

### Reconcile, don't patch

`hello` carries the provider's **entire** device list, and anything the database
still has for that provider which the provider no longer reports becomes
`absent`. A provider that crashed mid-change therefore cannot leave a stale row
behind. The same stance drives the `stream` subscription (below).

### Failure semantics

| Event | Result |
|---|---|
| Socket drops | Provider → `offline`, its devices → `absent`, its active reservations released |
| Heartbeat missed 3× | Treated as a drop — otherwise a half-open TCP connection keeps devices looking `ready` forever |
| Provider offline | `device.reserve` fails `PRECONDITION_FAILED` rather than handing out an unreachable device |
| Unparseable message | Socket rejected and closed; the two sides disagree about the contract |

## Session tokens ✅ built

Ed25519 (`EdDSA`) JWTs signed by the coordinator, published at
`/.well-known/farm-jwks.json`, verified by each provider against its cached copy.

```json
{ "deviceId": "…", "userId": "…", "reservationId": "…", "providerId": "…",
  "iss": "<PUBLIC_URL>", "aud": "farm-provider", "exp": "≈60s" }
```

`device.sessionToken` issues one only to the holder of the device's active
reservation, and returns the `wss://…/s/<deviceId>` URL alongside. The short
`exp` is a **backstop**: revocation is a `session.revoke` push, which is instant
and does not wait for a token to age out.

### The issuer is told, not inferred

`iss` is the coordinator's **public** URL — the origin browsers use — and it
arrives in `hello.ack`. A provider cannot derive it from the address it dialled:
those match only on a laptop, and in any real deployment the provider reaches
the coordinator over an internal address while tokens are signed with the public
one. Inferring it meant every session was refused with `InvalidIssuer`, visible
to the browser as nothing but an abnormal socket close.

## Session plane — browser ↔ provider ✅ built

`wss://<publicBaseUrl>/s/<deviceId>?token=<jwt>`

| Direction | Messages |
|---|---|
| browser → provider (`ClientMessage`) | `pointer.down/move/up`, `key`, `text`, `clipboard.get/set`, `rotate`, `keyframe`, `pong` |
| provider → browser (`ServerMessage`) | `codec` handshake, `display`, `clipboard`, `install.progress`, `install.result`, `session.closed`, `error`, `ping` |

Pointer coordinates are **normalised 0..1, not pixels**: the browser needs no
knowledge of the true resolution, and a mid-session rotation cannot
desynchronise an in-flight event.

`pointer.*` is deliberately three messages rather than one `tap`. The provider's
`Pointer` state machine needs down/move/up to distinguish a drag from a tap — the
regression `stf-ios-provider/src/control.rs` documents at length.

## Artifact plane — browser → provider ✅ built

Same port, same token. `POST /s/:deviceId/install`,
`GET /s/:deviceId/screenshot.png`, `GET /s/:deviceId/mjpeg`.

`/mjpeg` is the degraded fallback for browsers that cannot decode the real
stream: `multipart/x-mixed-replace` at ~3fps. Its parts are **PNG** despite the
path — both backends capture PNG, and converting would mean a transcode in the
provider, which happens nowhere in this system.

### CORS is structural here, not incidental

The browser loads the app from the coordinator's origin and then talks straight
to the provider — so *every* artifact-plane request is cross-origin, in every
deployment. Without CORS this plane is unreachable from a browser while looking
perfectly healthy to `curl`, which is exactly how phase 5 found it missing.

The allowed origins ride in `hello.ack` rather than provider config: the
coordinator owns policy, and a separately configured provider would drift the
first time the web app moved. Until a provider registers the list is empty and
it refuses browser requests — it fails closed, then self-heals on registration.

Credentials are never allowed. Authorization on this plane is the session token
in the query string, never a cookie, so there is no ambient authority for a
hostile page to ride on even if an origin were mislisted.

## Fake provider ✅ built

`packages/protocol/test/fake-provider.ts` speaks the whole control plane with no
hardware. It is both the integration-test harness and a dev tool:

```bash
bun packages/protocol/test/fake-provider.ts --token pft_… --id lab-1 --devices 4
```

Register a provider and issue a token under `/admin/providers` first. It reports
four synthetic devices with realistic identifiers, geometry and codecs, answers
commands with plausible data, and exposes `received[]` for assertions.
`packages/coordinator/test/gateway.test.ts` drives it over a real socket against
a real database.
