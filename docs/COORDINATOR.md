# `packages/coordinator`

Hono app on Bun. The app plane and the control plane; never the data plane.

```
src/
├── index.ts            Bun.serve entrypoint + signal handling
├── app.ts              Hono routes: /health, JWKS, gateway, auth, tRPC
├── env.ts              t3-env + zod, validated at boot — misconfig fails fast
├── auth.ts             better-auth instance
├── db.ts               shared pool + drizzle client
├── cli/grant-admin.ts  first-admin bootstrap
├── lib/
│   ├── audit.ts        audit-row helper (never throws into the caller)
│   ├── events.ts       device-change broadcast, coalesced
│   ├── provider-token.ts  generate/hash machine credentials
│   └── session-token.ts   Ed25519 signer + published JWKS
├── gateway/            provider control plane (see below)
└── trpc/
    ├── init.ts         context + public/protected/admin procedures
    ├── router.ts       root router
    └── routers/        user · device · provider · admin · stream
```

## Environment

Everything is validated by `env.ts` at import time — the process refuses to
start on a bad config rather than failing on the first request. See
`.env.example` for the full list. The ones with sharp edges:

| Var | Why it matters |
|---|---|
| `PUBLIC_URL` | Must match the OAuth redirect URI host, or Microsoft sign-in fails at the callback |
| `WEB_ORIGIN` | Comma-separated. Credentialed CORS allowlist for the SPA in development |
| `AUTH_SECRET` | ≥32 chars. Rotating it invalidates every session |
| `SESSION_TOKEN_PRIVATE_KEY` | Ed25519 PKCS#8 PEM. **Generated in memory if unset**, which invalidates live device sessions on every restart. Set it in production |
| `ENABLE_EMAIL_PASSWORD` | Bootstrap path. Turn off once Microsoft sign-in works |
| `APP_NAME` | The one place the product name lives — see RENAMING.md |

## Authentication

better-auth with the Drizzle adapter.

- **Microsoft / Entra ID** via `socialProviders.microsoft`. Configured only when
  both `MICROSOFT_CLIENT_ID` and `MICROSOFT_CLIENT_SECRET` are present; the
  `user.capabilities` procedure reports what's live so the sign-in page renders
  only the methods that actually work.
- **Email + password** for bootstrapping and local development.
- **`admin()` plugin** for user listing, roles, bans and impersonation. Role and
  ban *mutations* go through better-auth's client API rather than a tRPC
  procedure, because the plugin owns the session invalidation that must follow
  them.

### The cookie-cache gotcha

`session.cookieCache` is on with a 60s max age, so a role change (e.g.
`grant-admin`) is not visible to the affected user for up to a minute. Signing
out and back in is immediate. This is a deliberate latency/DB-load trade — if it
ever becomes a problem, drop `cookieCache` rather than shortening it further.

## tRPC

v11, fetch adapter, mounted at `/api/trpc`. Three procedure levels in
`trpc/init.ts`:

- `publicProcedure` — no session required
- `protectedProcedure` — session required; rejects banned users
- `adminProcedure` — `role === "admin"`

### Routers

| Router | Procedures | Status |
|---|---|---|
| `user` | `me`, `capabilities` | ✅ |
| `user` | `adbKeys.list`/`add`/`remove` | ✅ |
| `device` | `list`, `get`, `reserve`, `renew`, `release`, `myReservations` | ✅ |
| `device` | `sessionToken`, `apps`, `launch`, `uninstall`, `reboot`, `rotate`, `adbExpose`, `adbUnexpose` | ✅ |
| `device` | `requestJoin`, `cancelJoinRequest`, `answerJoinRequest`, `myJoinRequest`, `leaveSession` | ✅ |
| `device` | `answerAdbAuthRequest` | ✅ |
| `provider` | `list`, `create`, `update`, `remove`, `restartDevice` | ✅ |
| `provider` | `tokens.list`/`create`/`revoke` | ✅ |
| `admin` | `users`, `forceRelease`, `joinSession`, `audit` | ✅ |
| `stream` | `devices` — live inventory updates over SSE | ✅ |

`stream.devices` yields a **revision counter, not device data**. Clients respond
by invalidating `device.list`, so what they render always came from one
consistent read. That costs an extra round trip per change and removes an entire
class of "the UI drifted from the database" bugs. The SPA falls back to 5s
polling when the stream is not connected, and labels which mode it is in.

### Being in someone else's session

A device held by another person is not a dead end. Two ways in, one destination:

- `admin.joinSession` — an admin lets themselves in.
- `device.requestJoin` → `device.answerJoinRequest` — everyone else asks, and
  **the holder** answers. (An admin may answer too; they can already join and
  force-release, so withholding the smaller power would be theatre.)

Both end in one open `reservationObserver` row, and **that row is the
authorization** — not the caller's role. `device.sessionToken` mints against the
*holder's* `reservationId` with the joiner's own `userId`, and
`requireOwnedDevice` accepts holder, observer or admin, so someone let in can
drive the device rather than only watch it. The provider matches sessions on
`reservationId` and needs no change for any of it.

Requests expire (`JOIN_REQUEST_TTL`, `lib/reservations.ts`) and do not survive
their reservation: `releaseActive` retires every pending row alongside the
observer rows it closes.

`device.leaveSession` is `protectedProcedure`, not admin — it closes *your own*
observer row and nobody else's. It lived on the admin router back when only
admins could be in a session, which made the Leave button 403 for the first
non-admin who was ever let into one.

Releasing is also where a device is held back for cleaning: when the policy is
on and the provider is connected, `releaseActive` sets `cleaning` instead of
`ready` and sends `device.cleanup` after `session.revoke`. The provider returns
the device with a `device.status`. Nothing waits — see [CLEANUP.md](CLEANUP.md).

### Who may `adb connect`

The coordinator decides; the provider enforces. It never answers a per-connection
question — a provider must keep serving an authorised session across a
coordinator restart, exactly as it does on the session plane.

The entitled set for a reservation is the holder's keys plus those of everyone
still present as an observer (`lib/adb-keys.ts`). It rides out with
`session.authorize` and is re-pushed as `device.adb.keys` whenever it changes: a
key added or removed, somebody joining or leaving. **Always the whole set,
never a delta** — a dropped message can leave a provider with a stale set, but
never with a revoked key. Adding or removing a key fans out only to the
reservations that user is actually in.

A key nobody recognises parks on the provider and arrives as `adb.auth.request`.
Those live in **memory** (`lib/adb-auth.ts`), not a table: each is bound to a
socket held open on a provider, so a coordinator restart means the connection
was already refused and a persisted row could only describe something nobody can
approve. They live 120 seconds, and are dropped when the provider disconnects or
the reservation ends. This is the opposite call from `joinRequest`, which is a
table precisely because the requester's browser outlives a socket blip.

`device.answerAdbAuthRequest` is the holder's answer (an admin may answer too,
as with join requests). Approving registers the key **on the answerer's own
account** and admits that connection with `adb.auth.decision`; the
`device.adb.keys` refresh that follows is what makes the *next* connection
silent. Admission deliberately does not depend on the refresh arriving first.

**Releasing clears `device.adbPort`.** The provider withdraws the bridge itself
on `session.revoke`, so the column is the only place a dead connect string
could survive — the UI would go on offering a port nobody is listening on.

The durable trace is the `auditLog` row. `user.adbKeys.add` returns a 409 on a
fingerprint already registered — the index is global, because a key identifies
one person or "who ran this `adb shell`" has no answer.

## The reservation reaper

`startReservationReaper` sweeps every 30s and releases on three conditions —
lapsed `expiresAt`, idle `lastActivityAt`, and `startedAt` past the maximum
duration, the latter two off unless an admin configured them. Every release goes
through `releaseActive`, which is the one place that pushes `session.revoke` and
writes the audit row.

It also retires unanswered join requests, which release nothing and so sit
outside those three conditions, and returns any device stuck in `cleaning` past
its deadline plus a minute. That last sweep only ever fires when a provider died
mid-clean — the provider guarantees it leaves that state under its own deadline
— and exists because a device nobody can reserve is worse than one that did not
finish being wiped.

## The provider gateway

`GET /api/providers/connect`, upgraded to a WebSocket. Wire details are in
[PROTOCOL.md](./PROTOCOL.md); what matters here is the code shape.

```
src/gateway/
├── route.ts      Hono upgrade + pre-upgrade bearer auth
├── handler.ts    GatewaySession — the state machine
└── registry.ts   ProviderConnection + the live-socket registry
```

`GatewaySession` takes `send` and `close` as constructor arguments rather than
holding a WebSocket, so the entire state machine is exercisable without a real
socket. `ProviderConnection.command()` correlates a request with its
`command.result` and bounds it with a 15s timeout — a provider that accepts a
command and never answers must not wedge a tRPC caller.

Bun needs the socket handlers at the server level, so `gatewayWebSocket` is
passed to `Bun.serve({ websocket })` in `src/index.ts`. Omitting it makes every
upgrade silently fail.

### The registry is in-memory

`providers` maps provider id → live connection, per coordinator process. Two
coordinator instances would each be blind to the other's providers. Since
providers dial *out*, this is not a transparent scale-out — it needs Postgres
`LISTEN`/`NOTIFY` or a bus first. Same applies to `lib/events.ts`.

## Bootstrapping the first admin

Promoting a user is an admin-only action, so an empty farm could never get its
first admin. The account created on an empty `user` table is therefore made
`admin` automatically, in a better-auth `user.create.before` hook (`auth.ts`).

Every case after that — a farm whose only admin left, a promotion done out of
band — uses the CLI:

```bash
bun --env-file=.env packages/coordinator/src/cli/grant-admin.ts you@example.com
```

Sign in once first so the user row exists.

## Testing

```bash
docker compose -f docker-compose.dev.yml up -d
bun test --env-file=.env packages/coordinator/test
```

`reservation.test.ts` runs tRPC routers through `createCaller` against a real
Postgres — the reservation guarantees are a database property, so testing them
against a mock would test nothing.

`gateway.test.ts` goes further: it boots the real Hono app on a port and drives
it with the fake provider over a real WebSocket. The state machine's whole job is
coordinating an app, a socket and a database, so mocking any of the three would
test nothing either.
