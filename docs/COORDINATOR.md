# `packages/coordinator`

Hono app on Bun. The app plane and the control plane; never the data plane.

```
src/
├── index.ts            Bun.serve entrypoint + signal handling
├── app.ts              Hono routes: /health, /api/auth/*, /api/trpc/*
├── env.ts              t3-env + zod, validated at boot — misconfig fails fast
├── auth.ts             better-auth instance
├── db.ts               shared pool + drizzle client
├── lib/audit.ts        audit-row helper (never throws into the caller)
├── cli/grant-admin.ts  first-admin bootstrap
└── trpc/
    ├── init.ts         context + public/protected/admin procedures
    ├── router.ts       root router
    └── routers/        user · device · provider · admin
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
| `device` | `list`, `get`, `reserve`, `renew`, `release`, `myReservations` | ✅ |
| `device` | `sessionToken`, `apps`, `launch`, `uninstall`, `reboot`, `rotate`, `adbExpose` | phase 2–4 |
| `provider` | `list` | ✅ |
| `provider` | `tokens.create`/`revoke`, `restartDevice` | phase 2 |
| `admin` | `users`, `forceRelease`, `audit` | ✅ |
| `stream` | `subscribe` — live device-list updates | phase 2 |

Until `stream` lands the SPA polls `device.list` every 5s. That is a known
placeholder, marked in the code.

## Not yet built

- `/api/providers/connect` — the provider gateway WebSocket and its
  control-plane state machine (hello → reconcile → heartbeat → events →
  commands). Phase 2.
- `/.well-known/farm-jwks.json` — Ed25519 public key for session tokens.
  Phase 2.
- The reservation reaper — expires lapsed reservations and pushes
  `session.revoke` to the owning provider. Phase 6.

## Bootstrapping the first admin

Promoting a user is itself an admin-only action, so the first one comes from the
CLI:

```bash
bun --env-file=.env packages/coordinator/src/cli/grant-admin.ts you@example.com
```

Sign in once first so the user row exists.

## Testing

```bash
docker compose -f docker-compose.dev.yml up -d
bun test --env-file=.env packages/coordinator/test
```

The suite runs tRPC routers through `createCaller` against a real Postgres — the
reservation guarantees are a database property, so testing them against a mock
would test nothing.
