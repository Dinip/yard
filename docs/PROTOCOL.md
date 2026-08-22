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
bun run protocol:gen     # → packages/provider/crates/yard-protocol/src/generated.rs
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
asserts zod parses them and writes `crates/yard-protocol/tests/fixtures.json`
(a generated file, so biome ignores it — otherwise running the tests and running
the formatter would each undo the other);
`cargo test -p yard-protocol` reads it back and asserts serde re-encodes it
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
| `adb.auth.request` | An `adb connect` offered a key the provider has not been told about; its connection is parked pending an answer |
| `install.finished` | Written to `auditLog` — the file is already deleted |
| `cleanup.finished` | Written to `auditLog` — what a between-users reset removed, cleared, wiped and failed at |
| `file.pulled` | Written to `auditLog` — bytes left the device and the coordinator was not on the path |

### coordinator → provider (`CoordinatorMessage`)

`hello.ack` (carries the heartbeat interval, the JWKS URL, the token `issuer`,
and `webOrigins` — the browser origins the provider may serve; see below),
`hello.reject`,
`ping`, `adb.auth.decision` (admits or refuses a parked `adb connect`), and
`command` — whose `payload` is one of:

`session.authorize` · `session.revoke` · `device.reboot` · `device.rotate` ·
`device.apps` · `device.launch` · `device.uninstall` · `device.cleanup` ·
`device.adb.expose` · `device.adb.unexpose` · `device.adb.keys` ·
`device.restart`

Every command is correlated by id and bounded by a 15s timeout — a provider that
accepts a command and never answers must not wedge the caller.

`device.cleanup` is the one deliberately sent fire-and-forget, because a
multi-package uninstall runs far past that timeout. It carries the farm's
`CleanupSteps`, an `AppFilter` scoping which apps `clearAppData` may touch, and
a deadline; the device's return to `ready` arrives as an ordinary
`device.status`, and the report as `cleanup.finished`. See
[CLEANUP.md](CLEANUP.md).

### ADB authentication

`adb connect` is authenticated by the **provider**, against keys the coordinator
tells it about — the device itself only ever trusts the provider's own key. See
[PROVIDER.md](PROVIDER.md) for the bridge that does it.

`session.authorize` carries `adbKeys`, the set entitled to that session: the
holder's keys plus those of anyone approved into the session. `device.adb.keys`
replaces that set when it changes. A key the provider has never seen produces an
`adb.auth.request`, the connection parks for 120s, and the holder's answer comes
back as `adb.auth.decision`. Any of a timeout, a refusal or a dropped control
plane closes the connection.

`AdbKey` carries the whole public key, not just a fingerprint, because ADB
authentication is challenge-response: the provider issues a token and verifies a
signature over it, which it cannot do from a fingerprint.

### Reconcile, don't patch

`hello` carries the provider's **entire** device list, and anything the database
still has for that provider which the provider no longer reports becomes
`absent`. A provider that crashed mid-change therefore cannot leave a stale row
behind. The same stance drives the `stream` subscription (below), and
`device.adb.keys`, which is always the whole set — a lost delta would leave a
provider trusting a key an admin deleted.

### Failure semantics

| Event | Result |
|---|---|
| Socket drops | Provider → `offline`, its devices → `absent`, its active reservations released |
| Heartbeat missed 3× | Treated as a drop — otherwise a half-open TCP connection keeps devices looking `ready` forever |
| Provider offline | `device.reserve` fails `PRECONDITION_FAILED` rather than handing out an unreachable device |
| Unparseable message | Socket rejected and closed; the two sides disagree about the contract |

## Session tokens ✅ built

Ed25519 (`EdDSA`) JWTs signed by the coordinator, published at
`/.well-known/yard-jwks.json`, verified by each provider against its cached copy.

```json
{ "deviceId": "…", "userId": "…", "reservationId": "…", "providerId": "…",
  "iss": "<PUBLIC_URL>", "aud": "yard-provider", "exp": "≈60s" }
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

`key` carries a free-form name, so hardware buttons need no new message type.
The vocabulary the browser sends:

| Name | Meaning | Android | iOS |
|---|---|---|---|
| `Home` | the **hardware** button | `KEYCODE_HOME` | HID consumer `0x40` |
| `Back` | the hardware button | `KEYCODE_BACK` | — |
| `AppSwitch` (or `Recents`) | task switcher | `KEYCODE_APP_SWITCH` | — |
| `MoveHome` / `MoveEnd` | the **keyboard's** Home/End | `KEYCODE_MOVE_HOME/_END` | HID `0x4A`/`0x4D` |
| `Enter`, `Backspace`, `Tab`, `Escape`, `Arrow*`, `PageUp`, `PageDown`, `Delete` | as named | ✓ | ✓ |

The Home/MoveHome split is not cosmetic. The browser used to send the keyboard's
Home key as `Home`, which Android maps to `KEYCODE_HOME`: pressing it while
typing threw the device to the launcher. There is a regression test on each side.

A hardware button is sent as a **down/up pair**. Android injects a keycode per
edge and would otherwise leave the key held; iOS presses and releases on the
down edge and discards the up. Printable characters do not use `key` at all —
they go as `text`, which is also what carries IME output and paste.

## Artifact plane — browser → provider ✅ built

Same port, same token. `POST /s/:deviceId/install`,
`GET /s/:deviceId/screenshot.png`, `GET /s/:deviceId/mjpeg`,
`GET /s/:deviceId/files`, `GET /s/:deviceId/file`.

`/mjpeg` is the degraded fallback for browsers that cannot decode the real
stream: `multipart/x-mixed-replace` at ~3fps. Its parts are **PNG** despite the
path — both backends capture PNG, and converting would mean a transcode in the
provider, which happens nowhere in this system.

### Files off a device

`GET /s/:deviceId/files?path=…` answers a `FileListing` — the plane's first
schema'd response, in `packages/protocol/src/files.ts` so the provider
serialises a generated struct rather than a hand-rolled one. `path` absent means
"wherever this backend opens", so the browser never has to know that Android
means `/sdcard` and iOS means the AFC media root. A backend with no filesystem
access answers **501**, which is a different thing from an empty directory and
is rendered as such.

`GET /s/:deviceId/file?path=…` answers the bytes. The provider copies the file
into its scratch directory, serves it, and deletes it when the response body is
dropped — the same staging the install path uses in the other direction, and for
the same reason: a video off a phone must never sit in the provider's memory.
There is still no artifact storage anywhere.

**This is the only operation in the system that carries data out of a device**,
so it is the one that must be audited, and the audit row is written *before* the
body is sent: a download the client aborts half way still took the bytes off the
device. Directory listings are not audited — one browse would write a row per
click, and the row worth keeping is the one with the digest on it.

A directory the device refuses answers **502 with the device's own words**.
"Permission denied" is the whole answer for an unrooted adb reading
`/data/data`, and rewording it would only hide which path said no.

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
