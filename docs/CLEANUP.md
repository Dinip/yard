# Cleaning a device between users

A device released by one user must not arrive at the next one carrying their
apps, their signed-in accounts, their files and their rotated screen. Cleanup is
the step between `busy` and `ready`.

It is **off by default**. Turning it on is a farm-wide policy decision made in
`/admin/settings`, because what counts as "clean" depends on what the devices
are for — a farm of scratch handsets wants everything wiped, a farm with a
preinstalled test harness on every phone does not.

## What STF does

Worth reading first, because the shape of the problem is unchanged and STF's
version has been in production for years. Both checkouts described in
`REFERENCES.local.md` were read; the file is
`lib/units/device/plugins/cleanup.js`, 50 lines in our 3.7.1 fork and 163 in
upstream 3.7.9.

- One `adb.getPackages()` snapshot at **device-worker boot**, held in closure
  memory as `initialPackages`, plus the STF service package whitelisted.
- On `group.on('leave')`, list packages again and uninstall
  `_.difference(current, initial)`.
- Upstream adds three opt-in extras: `--cleanup-disable-bluetooth`,
  `--cleanup-bluetooth-bonds`, and `--cleanup-folder`, the last being a
  top-level `rm -rf` of configured directories with a hardcoded allowlist for
  its own minicap/minitouch binaries and a **startup** throw if a configured
  path begins with `/system`, `/boot`, `/proc`, `/vendor`, `/dev` or `/sys`.
- Separately, other plugins hang off the same `leave` event: `group.js` presses
  HOME, thaws rotation and releases the wake lock (`--screen-reset`, default
  on); `logcat`, `forward`, `connect` and `vnc` tear their sessions down;
  `mute.js` restores the mute state.

Despite the CLI help text promising "resetting accounts and clearing caches",
there is no `pm clear`, no account removal, no permission reset and no
`settings put` anywhere in the release path. iOS has none of it at all —
`--cleanup` is parsed and forwarded and nothing reads it, and the leave hooks in
`lib/units/ios-device/plugins/group.js` are commented out.

### The four bugs not to inherit

1. **The device is freed before cleanup starts.** `group.js` pushes
   `LeaveGroupMessage` — which clears the owner in the database — and *then*
   emits `leave`; `cleanup.js` never returns its promise chain to anyone. The
   device is advertised as available while `pm uninstall` and `rm -rf` are still
   running, so the next user can reserve a device mid-wipe and have apps
   uninstalled under them.
2. **Nothing knows whether it worked.** Every error is logged at warn level and
   swallowed. No timeout, no retry, no metric, no record, and no way for an
   admin to find out that a device has been failing to clean for a month.
3. **The baseline is per-worker-boot, not per-session.** A worker restart
   re-snapshots against the *dirty* package set, so anything left behind is
   blessed permanently. There is an abandoned 2016 branch
   (`origin/improve-device-cleanup`, commit `fcd0d150`) that moved the baseline
   into RethinkDB to fix exactly this. It was never merged.
4. **There is no state that means "cleaning".** Availability is computed as
   `present && ready && !using && !owner`, and the wire protocol's device status
   enum carries only ADB states. There is nowhere to put the answer.

## How this works instead

The architecture already supplies what STF lacked. `releaseActive()` in
`packages/coordinator/src/lib/reservations.ts` is a single chokepoint that all
four release paths funnel through, the device status enum is ours to extend, and
the provider owns the device outright.

```
release  (holder · admin force · reaper ×3 · provider disconnect)
   │
   └─ releaseActive()
        ├─ reservation → released, observers closed, join requests expired
        ├─ device: busy → cleaning              (was: busy → ready)
        ├─ session.revoke  → provider
        └─ device.cleanup  → provider

                provider
                   ├─ device.status { cleaning }
                   ├─ …steps, sequentially, under a deadline…
                   ├─ cleanup.finished { removed, cleared, wiped, errors }
                   └─ device.status { ready }
```

`device.cleanup` is sent with `commandNoWait`. The correlated `command()` path
has a 15s ceiling (`COMMAND_TIMEOUT_MS` in `gateway/registry.ts`) and a
multi-package uninstall blows straight through it; completion instead arrives as
a status push, the same way `device.reboot` already reports. That also keeps the
reconcile-don't-patch invariant intact — a dropped `cleanup.finished` costs an
audit row, not a stuck device.

Cleanup is **skipped entirely** when the provider is already gone
(`releaseActive({ revoke: false })`, the disconnect path). There is nothing to
send the command to, and the device is about to be marked `absent` anyway.

### Nothing can strand a device

A device stuck in `cleaning` is worse than a dirty device: it is invisible
inventory. Two independent guards, either of which is sufficient:

- **On the provider**, the cleanup task runs under a `tokio::time::timeout`.
  Success, step failure, or deadline, it ends by setting the device `ready` —
  or `unhealthy`, if the backend says so. The task cannot exit any other way.
- **On the coordinator**, a reaper sweep alongside the three reservation sweeps
  forces any device that has sat in `cleaning` for longer than the configured
  timeout plus a minute back to `ready`, with a warning. This covers a provider
  that died mid-clean.

A provider restart self-heals without either: `onClose` marks its devices
`absent` and the `hello` reconcile re-reports whatever is actually true.

### What counts as "installed during the session"

The provider snapshots `backend.apps()` when it receives `session.authorize` and
diffs against it at cleanup. On Android that snapshot is already
`pm list packages -3` — third-party only, which is exactly the set worth
uninstalling.

The snapshot is taken **only for a reservation id it has not seen**, so a renew
does not re-baseline. It is held in memory, and **if there is no baseline the
uninstall step is skipped and logged** rather than run against an empty set.
That is the deliberate inverse of STF's third bug: STF's missing baseline blesses
leftovers, ours declines to act. Losing a baseline means a provider restarted
mid-session, which is rare, and the failure mode of "one dirty device, logged"
beats "every preinstalled app removed from a rack".

Persisting the baseline — STF's abandoned `fcd0d150` — is the obvious follow-up
if that turns out to matter in practice.

## Steps

| Step | Setting | Default | Android | iOS |
|---|---|---|---|---|
| Uninstall apps installed during the session | `cleanup.uninstallApps` | on | `pm uninstall` | `AppService` uninstall |
| Reset screen state — HOME, rotation, clipboard | `cleanup.resetScreen` | on | keyevent + rotate + clipboard | home button + rotate |
| Clear data of surviving third-party apps | `cleanup.clearAppData` | **off** | `pm clear` | unsupported |
| Wipe configured scratch directories | `cleanup.wipeFolders` | **off** | `rm -rf` per entry | staged `.ipa` |

Plus `cleanup.enabled` (off), `cleanup.timeoutSeconds` (120), and the two
pattern lists that scope `clearAppData` below.

`clearAppData` is off by default because `pm list packages -3` includes anything
the organisation preinstalled — a test harness, an MDM agent — and wiping its
data is a decision, not a default. iOS has no equivalent operation at all; the
step reports `Unsupported` and cleanup continues.

### Scoping which apps get cleared

`clearAppData` alone means *every* surviving third-party app, which is rarely
what anyone wants: clearing a signed-in MDM agent or a test harness that holds
its own credentials breaks the device for everyone who reserves it afterwards.
Two pattern lists narrow it, sent to the provider with the command:

| Setting | Meaning |
|---|---|
| `cleanup.clearAppDataAllow` | Only these apps may be cleared. Empty means all of them. |
| `cleanup.clearAppDataDeny` | These are never cleared. Checked first, so it wins. |

Patterns are app ids where `*` matches any run of characters, dots included, and
matching is case-insensitive: `*.google.*` covers `com.google.android.gm`,
`com.acme.*` covers a whole vendor prefix, and a pattern with no `*` is a plain
equality test. There is no `**`, no character class and no path semantics —
these match app ids, not files.

**Prefer the allow list.** An allow list of "the apps under test, plus
`*.google.*`" keeps working when someone preinstalls a fifth thing next month;
a deny list has to be edited every time the fleet changes, and the cost of
forgetting is a broken device rather than a slightly dirty one. The deny list
exists for the narrower job of carving one app out of a prefix you allowed.

An app the filter skips is not a failed step. Nothing is recorded, because
skipping it is the policy working.

The lists apply to `clearAppData` only. `uninstallApps` needs no equivalent: it
removes the diff against the session's baseline, so an app that was on the
device before the user arrived is already out of its reach.

Steps run **sequentially**, never concurrently. STF learned this one the hard
way and left the commit message behind: "do only one adb command at a time to
ensure they all are executed". A step that fails is recorded in the report and
the remaining steps still run.

## Where configuration lives, and why it is split

**Which steps run** is farm-wide policy, so it lives in the coordinator's
settings table and is edited in `/admin/settings` like the reservation policy
next to it. The coordinator sends the enabled set with each `device.cleanup`.
The `clearAppData` patterns go with it: the worst a wrong pattern can do is
clear an app's data or fail to, which is the step's own blast radius and not a
wider one.

**Which folders get wiped** lives in the provider's YAML, per device. Two
reasons: the paths are specific to a device's platform and role, and — the real
one — a text field in a web form that ends in `rm -rf` on a phone should not
exist. STF's prefix guard is ported and made a hard config error, so a provider
configured to wipe `/system` fails at startup rather than on a device:

```yaml
devices:
  - udid: R5CT30XXXXX
    backend: android
    options:
      cleanup_paths:
        - /sdcard/Download
        - /sdcard/DCIM/Camera
```

The provider intersects the coordinator's step set with what its backend
supports and what its own config allows. A step nobody configured is a no-op,
not an error.

## Reading the results

Every completed cleanup writes one `device.cleanup` audit row, visible under
Devices in `/admin/audit`, carrying what was removed, what was cleared, what was
wiped, and any step errors. Since there is no artifact storage anywhere in this
system, that row is the only record a cleanup ever happened — the same
arrangement as installs.

## See also

[COORDINATOR.md](COORDINATOR.md) for the release path and the reaper ·
[PROVIDER.md](PROVIDER.md) for the backend trait and `cleanup_paths` ·
[PROTOCOL.md](PROTOCOL.md) for `device.cleanup` and `cleanup.finished` ·
[DATA-MODEL.md](DATA-MODEL.md) for the device status enum.
