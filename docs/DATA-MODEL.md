# Data model

Drizzle + Postgres. Schema lives in `packages/db/src/schema/`, split into
`auth.ts` (owned by better-auth) and `farm.ts` (owned by us). Migrations are
generated, never hand-written:

```bash
bun run db:generate   # after editing schema — writes packages/db/drizzle/*.sql
bun run db:migrate    # apply
```

**Every timestamp column is `timestamp without time zone` holding UTC.** Drizzle
serializes a `Date` to ISO, so column assignment is always right; a `Date`
interpolated into a raw ``sql`` `` fragment is **not** — the driver renders it
with the process's local offset, which Postgres then drops on the way into the
column, silently storing local wall clock. Bind `date.toISOString()::timestamp`
in raw SQL. `reservation.lastActivityAt` shipped with this bug.

## better-auth tables (`auth.ts`)

`user`, `session`, `account`, `verification`, plus the `admin` plugin's
`role` / `banned` / `banReason` / `banExpires` columns on `user` and
`impersonatedBy` on `session`.

**Do not rename these tables or columns.** better-auth's Drizzle adapter matches
them by name; a rename requires configuring the adapter's mapping in
`packages/coordinator/src/auth.ts` at the same time.

## Farm tables (`farm.ts`)

### `provider`

| Column | Notes |
|---|---|
| `id` | Provider-chosen stable id |
| `publicBaseUrl` | **The URL the browser uses**, not the one the coordinator uses. The coordinator never dials a provider. |
| `status` | `online` \| `offline`, driven by the control-plane socket |
| `lastSeenAt` | Heartbeat |

### `providerToken`

Machine credentials, deliberately **outside better-auth**: providers are not
users, never carry a session, and are rotated by admins.

`tokenHash` stores sha256 of the presented secret; the plaintext is shown once
at creation and never again.

### `device`

Keyed by udid (iOS) or serial (Android) — stable across reconnects, so a device
that unplugs and returns keeps its history.

Lifecycle: `absent` → `present` → `preparing` → `ready` → `busy` → `cleaning` →
`ready`, with `unhealthy` as a side state.

- `absent` — the owning provider is gone, or the device unplugged
- `ready` — reservable
- `busy` — an active reservation exists
- `cleaning` — released, being reset by its provider before it is offered again.
  Only reached when cleanup policy is on; `updatedAt` is when it started, which
  is what the reaper measures against. See [CLEANUP.md](CLEANUP.md)

`streamCodec` holds the string handed straight to the browser's `VideoDecoder`
(`hev1.1.6.L93.B0` for iOS, `avc1.640028` for Android). `adbPort` is populated
only while an adb transport is exposed.

### `reservation`

The replacement for STF's device-owned group model.

```sql
CREATE UNIQUE INDEX reservation_one_active_per_device
  ON reservation (device_id) WHERE state = 'active';
```

That index is the entire exclusivity mechanism. Two concurrent `device.reserve`
calls both pass the "is it free?" read; one then loses the insert with a
`23505` unique violation, which the router translates to a tRPC `CONFLICT`.
There is no application-level lock, no advisory lock, and no transaction
isolation tuning to get wrong. `packages/coordinator/test/reservation.test.ts`
asserts exactly one winner out of three racing callers.

A *second* reserve by the **same** user renews rather than conflicts — this is
what makes the popout window work: reservation is per user+device, so a second
tab joins the same reservation instead of being locked out.

### `reservationObserver`

Somebody in a session that is not theirs. An **open row here — `left_at is
null` — is what authorizes a non-holder on the session plane**; the admin role
is one way to get one, not a substitute for it. `device.sessionToken` checks for
the row and mints a token carrying *the holder's* `reservationId` with the
joiner's own `userId`, which the provider accepts exactly like the holder's
second tab. The provider needs no notion of any of this.

```sql
CREATE UNIQUE INDEX reservation_observer_one_open_per_user
  ON reservation_observer (reservation_id, user_id) WHERE left_at IS NULL;
```

A table rather than a column because join and leave are events worth querying,
and because the holder's UI names who is present.

### `joinRequest`

Asking the holder to be let in — the path for everyone who cannot join by
themselves. Approving one inserts a `reservationObserver` row and does nothing
else, so an invited user arrives exactly the way an admin's self-join does.

Keyed on `reservation_id`, not `device_id`, so a request cannot outlive the
session it was aimed at: `releaseActive` expires every pending row in the same
statement that closes the observers.

```sql
CREATE UNIQUE INDEX join_request_one_pending_per_user
  ON join_request (reservation_id, user_id) WHERE state = 'pending';
```

The same partial-unique trick as the two above: a double-clicked button cannot
leave the holder two identical questions to answer. `state` distinguishes
`denied` from `cancelled` (the asker withdrew) and `expired` (nobody answered
within `JOIN_REQUEST_TTL`, swept by the reaper) — three different answers that
an audit log should not have to guess between.

### `userAdbKey`

`user_id`, `fingerprint`, `public_key`, `comment`, `title`, `created_at`,
`last_used_at`.

What makes `adb connect` work without enrolling anyone's key on a phone. The
provider authenticates the ADB connection itself and matches the offered key
against these rows; the only key a device ever trusts is the provider's own.

```sql
CREATE UNIQUE INDEX user_adb_key_fingerprint_idx ON user_adb_key (fingerprint);
```

Unique across the table, not per user: a key identifies one person. Two accounts
claiming the same key would make "who ran this `adb shell`" unanswerable, which
is the question the mechanism exists to answer.

`public_key` is the whole base64 blob from `~/.android/adbkey.pub`, not just the
fingerprint, because ADB authentication is challenge-response — the provider
issues a token and verifies a signature over it.

### `auditLog`

`actorUserId`, `action`, `targetType`/`targetId`, `metadata` (jsonb), `at`.

Installs are recorded here (device, filename, size, sha256, outcome) because
**there is no artifact table** — the uploaded file is already deleted by the time
the row is written.

## What is deliberately absent

**No `app` / artifact table.** Uploads are per-install and transient. An install
is a *request against a device*, not a row that outlives it. This removes object
storage, artifact GC, quota management, and a whole class of "which build is
this?" confusion from the system.

**No table of pending ADB approvals.** A holder being asked to approve an
unknown key is answering about a TCP connection parked on a provider right now.
If the coordinator restarts, the provider has already refused it, so a row could
only ever describe something nobody can approve. It is held in memory for its
120 seconds; the durable trace is the `auditLog` row on the decision. This is
the opposite call from `joinRequest`, which is a table because what waits there
is a browser, not a socket.

**No device-owned ownership state.** See ARCHITECTURE.md — the provider holds
only the currently authorized `reservationId`, pushed to it.
