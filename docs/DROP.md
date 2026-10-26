# YARD Drop implementation plan

> Status: Android is written end to end (increments 0 through 10) and the
> release workflow builds the APK. Increments 0 through 9 are signed off on
> hardware; increment 10's streaming download and reservation cleanup are the
> two things left to watch on a device. The iOS implementation now includes
> Runner, a native Share Extension, automatic reservation-bound USB delivery,
> cleanup, automated tests and unsigned IPA releases. Physical iPhone
> testing passed on an iPhone 13 with iOS 27 and seven-day Personal Team signing.
> The remaining source-app and rollout checks are listed below.

YARD Drop is a first-party Flutter companion that appears as an Android and iOS share
target. It accepts file attachments from any app and gives the tester two
choices:

- **Save on this device** copies files to Android Downloads or a location chosen
  through the iOS Files picker.
- **Send to YARD browser** puts the files in a short-lived device inbox that the
  current YARD session can download.

Android shipped first. The increment descriptions below preserve the original
acceptance plan, including the physical-device checks required before rollout.

## Constraints

The implementation must preserve the existing YARD architecture:

- File bytes travel directly between the device, provider and browser. The
  coordinator never carries them.
- There is no artifact table or server-side artifact storage.
- A file pulled through the provider remains authenticated and audited.
- Files from one reservation must not remain for the next user.
- Incoming files are opaque bytes. YARD Drop does not unpack, parse or execute
  them.
- File I/O is streamed. A large attachment must never become one byte array in
  Kotlin, Dart, Rust or browser JavaScript.

The first implementation targets Android 10 and newer, API level 29. Supporting
older Android storage APIs is a separate decision after the farm inventory has
been checked.

## Why the app belongs in this repository

The companion is a separate application, but its browser inbox, cleanup rules
and release compatibility are tied to YARD. Keeping both ends in one repository
allows a change to update the Flutter app, web UI and provider behavior in one
review.

It lives outside `packages/` so it does not become part of the Bun workspace:

```text
apps/
└── yard_drop/
    ├── README.md
    ├── pubspec.yaml
    ├── analysis_options.yaml
    ├── lib/
    ├── test/
    └── android/
```

Flutter gets its own CI job but uses the root YARD version. The root TypeScript
and Rust commands do not invoke Flutter.

## Versioning and release

YARD Drop uses the same semantic version as the coordinator, web application,
database packages and provider. The companion is part of the farm baseline and
depends on browser and provider behavior from the same repository. Keeping one
version makes it clear which mobile build belongs to a deployed farm release.

There is one release tag and one changelog:

```text
v0.4.0
CHANGELOG.md
```

An app-only `feat(drop)` or `fix(drop)` participates in the root release just
like a change to any existing package. This may rebuild the companion when its
source did not change, but that cost is smaller than maintaining independent
tags, release PRs and compatibility ranges for an internal application.

Release Please remains a single `.` component. Do not add an app component,
`exclude-paths`, a second manifest entry or a separate app changelog. When
Increment 1 creates the Flutter project, add its version to the root component's
existing `extra-files`:

```json
{
  "type": "yaml",
  "path": "apps/yard_drop/pubspec.yaml",
  "jsonpath": "$.version"
}
```

The release PR then writes the same value to the root packages, Cargo workspace
and Flutter `pubspec.yaml`:

```yaml
version: 0.4.0
```

Android and iOS use that semantic version. When iOS ships later, its first build
uses the current root YARD version rather than starting another version line.

### Mobile build numbers

The shared semantic version identifies the product release. Each signed mobile
artifact also needs a monotonically increasing CI build number. The release
workflow passes both values to Flutter:

```text
flutter build apk --build-name 0.4.0 --build-number 142
flutter build ipa --build-name 0.4.0 --build-number 142
```

Use one numeric value derived from the release workflow run number and attempt,
such as `run_number * 100 + run_attempt`. A retried signing or upload job must
receive a higher build number. Flutter maps the two values as follows:

| Flutter value | Android | iOS |
|---|---|---|
| build name `0.4.0` | `versionName` | `CFBundleShortVersionString` |
| build number `142` | `versionCode` | `CFBundleVersion` |

The application displays the semantic version, build number and source commit
on an About screen. CI puts the same values in the artifact name and build
metadata. Each artifact name is immutable and has a SHA-256 checksum.

### Release workflow

Keep the existing root Release Please output as the only release gate. When
`needs.release-please.outputs.released == 'true'`, the release workflow does two
independent jobs:

1. Publish the existing container images.
2. Call a reusable mobile workflow that builds and signs YARD Drop.

The jobs share a version and tag but do not share build steps or tooling. A
Flutter or signing dependency must not enter the container image job.

The mobile job attaches artifacts such as these to the root GitHub release:

```text
yard-drop-0.4.0+142-android.apk
yard-drop-0.4.0+142-android.apk.sha256
yard-drop-0.4.0+142-ios.ipa
yard-drop-0.4.0+142-ios.ipa.sha256
```

The mobile workflow checks for changes under `apps/yard_drop/` since the
previous release before building either platform. The IPA is unsigned; farm
operators sign the app and extension using their own Apple account. Call the reusable
mobile workflow from the existing release workflow because a tag created with
`GITHUB_TOKEN` does not start another tag-triggered workflow.

Implementation references:

- [Flutter Android version mapping](https://docs.flutter.dev/deployment/android#update-the-apps-version-number)
- [Flutter iOS version mapping](https://docs.flutter.dev/deployment/ios#update-the-apps-build-and-version-numbers)
- [Release Please YAML extra files](https://github.com/googleapis/release-please/blob/main/docs/customizing.md#updating-arbitrary-yaml-files)
- [Release Please root component outputs](https://github.com/googleapis/release-please-action#root-component-outputs)

### App version versus batch schema

Do not use the application version as the browser compatibility check. The
application may release several fixes without changing the on-device inbox
format.

Every Android and iOS browser batch contains a `batch.json` with an integer
`schemaVersion` and producer information:

```json
{
  "schemaVersion": 1,
  "producer": {
    "appVersion": "0.4.0",
    "buildNumber": 142,
    "commit": "abc1234"
  }
}
```

Additive changes keep the same schema version. A breaking change increments it.
The web application accepts the schema versions it understands and shows the
installed producer version in an unsupported-schema error. Save-to-device does
not depend on this schema and continues to work if the browser side is older.

The first implementation pins an exact signed companion artifact in the farm
baseline and verifies that version during provisioning. Automatic installation
and upgrades remain later work. Before adding fleet-wide drift detection, teach
the Android provider app listing to report `versionName` and `versionCode`; it
currently returns no Android application version.

## Android design

```text
Android ACTION_SEND or ACTION_SEND_MULTIPLE
                  │
                  ▼
        Kotlin share receiver
                  │
                  ├── validates content URIs
                  ├── streams them to private temporary storage
                  └── publishes metadata to Flutter
                                   │
                                   ▼
                         Flutter choice screen
                                   │
                       ┌───────────┴───────────┐
                       ▼                       ▼
              Save on this device      Send to YARD browser
                       │                       │
                       ▼                       ▼
        Download/YARD Drop/Saved   Download/YARD Drop/Inbox
                                               │
                                               ▼
                                    existing ADB file pull
                                               │
                                               ▼
                                       browser download
```

Kotlin owns URI access, temporary staging and MediaStore writes. Dart owns the
screen, state transitions and the user's choice. File contents never cross a
Flutter platform channel.

### Native share lifecycle

The Android activity handles both entry paths:

- `onCreate()` receives a share that starts a cold application.
- `onNewIntent()` receives a share while the application is already running.

The activity uses `singleTask` launch mode so a second share reaches the
existing task. A cold-start query is still required because Kotlin may receive
and stage the attachment before Dart subscribes to events.

The native layer copies every incoming URI into private temporary storage as
soon as it receives the intent. An `ACTION_SEND` URI grant is temporary, so the
user must not be able to leave a pending choice that still depends on the
source application keeping that grant alive.

### Platform channel contract

The Dart application owns these models:

```dart
enum IncomingShareState {
  receiving,
  ready,
  saving,
  saved,
  failed,
}

enum ShareDestination {
  downloads,
  browserInbox,
}

final class IncomingFile {
  final String id;
  final String displayName;
  final String? mimeType;
  final int? reportedSize;
}

final class IncomingShare {
  final String id;
  final DateTime receivedAt;
  final List<IncomingFile> files;
  final IncomingShareState state;
  final String? error;
}
```

The application wraps the platform channel behind a YARD-owned interface:

```dart
abstract interface class ShareGateway {
  Future<List<IncomingShare>> pendingShares();
  Stream<IncomingShare> get changes;

  Future<SaveResult> save(
    String shareId,
    ShareDestination destination,
  );

  Future<void> discard(String shareId);
}
```

The method channel exposes:

- `listPendingShares`
- `saveShare`
- `discardShare`
- `purgeExpiredShares`

An event channel reports ingestion, progress and completion. Dart always calls
`listPendingShares` on startup and resume, so an early event cannot lose a
share.

Suggested source layout:

```text
apps/yard_drop/
├── lib/
│   ├── main.dart
│   ├── app.dart
│   └── share/
│       ├── incoming_share.dart
│       ├── share_gateway.dart
│       ├── share_controller.dart
│       └── share_page.dart
├── test/
│   ├── share_controller_test.dart
│   └── share_page_test.dart
└── android/app/src/
    ├── main/
    │   ├── AndroidManifest.xml
    │   └── kotlin/<application-id>/
    │       ├── MainActivity.kt
    │       ├── IncomingShareBridge.kt
    │       ├── ShareIntentParser.kt
    │       ├── IncomingShareStore.kt
    │       └── MediaStoreWriter.kt
    ├── test/
    └── androidTest/
```

## Increment 0: settle Android identifiers and limits

Decide these values before scaffolding:

- The permanent Android application ID under an owned reverse domain.
- Minimum Android API level. The proposal is API 29.
- The maximum number of files in one share.
- The maximum size of one file and of the whole batch.
- How long an abandoned private staging batch may live.
- Whether the share target label is `Send to YARD` or the full product name.

The application ID must be stable. Changing it later creates a different app
and complicates upgrades on every farm device.

No code is committed in this increment. Its result is a short decisions section
in `apps/yard_drop/README.md` when Increment 1 creates that file.

## Increment 1: scaffold an Android-only Flutter app

Create the app without an iOS target:

```bash
flutter create \
  --platforms=android \
  --org <owned.reverse.domain> \
  apps/yard_drop
```

Add:

- The final application ID.
- The user-visible application name `YARD - Device Farm`.
- A small home screen explaining that a user must share a file from another
  app and choose `Send to YARD`.
- An About screen showing the semantic version, CI build number and source
  commit supplied by the build workflow.
- A Material 3 light and dark theme.
- A fake `ShareGateway` for widget and controller tests.
- Flutter output directories in the repository `.gitignore`.
- A separate `.github/workflows/flutter.yml` workflow.
- Flutter setup and commands in `docs/DEVELOPMENT.md`.

The initial CI job runs:

```bash
flutter pub get
flutter analyze
flutter test
flutter build apk --debug
```

Do not add Flutter commands to the root Bun scripts.

Acceptance criteria:

- `flutter run` opens the companion on Android.
- `flutter test` passes.
- CI produces a debug APK.
- Existing Bun and Cargo checks still run without a Flutter installation.

Suggested commit:

```text
feat(drop): scaffold the Android Flutter companion
```

## Increment 2: add the Dart share state and screen

Implement the models and `ShareGateway` contract above. Build the screen
against an in-memory fake before connecting Android.

The screen supports these states:

- No share is waiting.
- Android is receiving files.
- A single file is ready.
- A batch is ready.
- The selected destination is being written.
- The operation completed.
- One or more files failed.

Only `Save to Downloads` appears at first. `Send to YARD browser` remains out of
the UI until its storage behavior exists.

Acceptance criteria:

- Controller tests cover every state transition.
- Widget tests cover empty, ready, progress, success and failure screens.
- Platform-specific types do not appear outside the gateway implementation.

Suggested commit:

```text
feat(drop): define the incoming share workflow
```

## Increment 3: receive one Android file

Register an Android share target with `ACTION_SEND`, `CATEGORY_DEFAULT` and the
`*/*` MIME filter. An activity alias may give the share target the shorter
`Send to YARD` label while the launcher keeps the full product name.

`ShareIntentParser` performs these steps:

1. Confirm the action is `ACTION_SEND`.
2. Read a URI from `Intent.EXTRA_STREAM`.
3. Fall back to `ClipData` if the extra is absent.
4. Require a file attachment and reject a text-only share.
5. Accept a `content://` URI in the first implementation.
6. Query `DISPLAY_NAME` and `SIZE` through `ContentResolver`.
7. Query the MIME type through `ContentResolver`.
8. Generate a safe fallback filename when metadata is absent.
9. Pass metadata and ingestion state to Flutter.

Do not log a content URI, filename or file contents. Any of them may contain
customer data.

The parser must tolerate an unknown size, unknown MIME type, missing name and
incorrect metadata. A URI without a valid read grant produces a useful error.

Acceptance criteria:

- The company app shows `Send to YARD` in its share sheet.
- Selecting it opens the Flutter screen.
- The screen shows the file name, MIME type and size when available.
- A warm application receives a second share through `onNewIntent()`.
- A text-only share says that YARD Drop requires a file attachment.

Suggested commit:

```text
feat(drop): receive single Android file shares
```

## Increment 4: stage an incoming file privately

Copy the incoming content stream before asking the user where it should go:

1. Create `cacheDir/incoming/<share-id>/`.
2. Open the URI with `ContentResolver.openInputStream()`.
3. Stream it to an incomplete temporary name with a fixed-size buffer.
4. Count actual bytes instead of trusting the reported size.
5. Enforce the file and batch limits from Increment 0.
6. Rename the file only after the copy succeeds.
7. Write a small metadata record beside it.
8. Mark the share ready.
9. Remove the incomplete file on any error.

The native store keeps a queue rather than one `latestShare` slot. A new share
must not overwrite a previous batch that still awaits a decision.

At application startup, purge incomplete entries and completed staging entries
older than the configured expiry. Never purge files already published to
Downloads.

Do not add WorkManager or a background service. Keep the activity alive and
show a receiving state while the copy runs.

Acceptance criteria:

- The source URI is no longer needed after staging completes.
- Rebuilding or rotating the Flutter screen does not lose the share.
- An interrupted input stream leaves no complete-looking file.
- A file larger than the configured limit fails before filling the device.
- The copy uses bounded memory.

Suggested commit:

```text
feat(drop): stage incoming files before user action
```

## Increment 5: save one file to Downloads

Implement `MediaStoreWriter` for API level 29 and newer. The destination is:

```text
Download/YARD Drop/Saved/
```

For each file:

1. Sanitize the display name.
2. Set `DISPLAY_NAME`.
3. Set the supplied MIME type or `application/octet-stream`.
4. Set `RELATIVE_PATH` to `Download/YARD Drop/Saved`.
5. Set `IS_PENDING` to `1`.
6. Insert the row into `MediaStore.Downloads`.
7. Stream the staged file into the returned output stream.
8. Set `IS_PENDING` to `0` after the copy finishes.
9. Delete the MediaStore row if the copy fails.
10. Delete the private staged copy after success.

Do not request `MANAGE_EXTERNAL_STORAGE`. Do not request broad external-storage
read permission. YARD Drop receives a temporary grant for the source URI and
owns the MediaStore records it creates.

The Flutter screen shows:

- A file summary.
- A primary `Save to Downloads` action.
- A secondary `Cancel` action.
- Progress when the total size is known.
- The final folder and a Done action after success.

Acceptance criteria:

- Share the company ZIP to YARD Drop.
- Save it to Downloads.
- Open it from the Android Files app.
- Browse to `/sdcard/Download/YARD Drop/Saved` in YARD.
- Download it with the existing Files dialog.
- Compare the downloaded bytes with the source.

This is the first useful release.

Suggested commit:

```text
feat(drop): save shared files to Android Downloads
```

## Increment 6: support multiple and arbitrary files

Add `ACTION_SEND_MULTIPLE`. Combine and deduplicate URIs from `EXTRA_STREAM` and
`ClipData` while preserving their original order.

One share action becomes one batch. Every attachment is staged in that batch's
private directory.

Handle:

- Duplicate filenames in one batch.
- Mixed MIME types.
- Unknown sizes.
- One invalid URI among valid URIs.
- Partial save failures.
- A second incoming batch while another remains open.

Do not remove files that reached Downloads because a later item failed. Return
a result per item and keep failed staged items available for retry.

Acceptance criteria:

- Several files shared from Android Files appear as one batch.
- Duplicate names receive stable, safe suffixes.
- A failed item does not hide successful items.
- Failed files can be retried or discarded.
- Single-file `ACTION_SEND` still works.

Suggested commit:

```text
feat(drop): support multiple Android share attachments
```

## Increment 7: finish the Android save flow

### Automated tests

Dart tests cover:

- Empty, single-file and multiple-file states.
- Receiving and saving progress.
- Partial failure and retry.
- Cancellation and discard.
- A second incoming batch.

Android instrumentation tests use a test `ContentProvider` that can return:

- A valid finite stream.
- An unknown length.
- No display name.
- A stream that fails partway through.
- A URI without a read grant.
- A large generated stream without retaining it in memory.
- A copy cancelled partway, which must leave no staged batch and no prompt.

Instrumentation tests live in `apps/yard_drop/android/app/src/androidTest`. They
need a real device, so they are run by hand rather than in CI — see
[Releasing the APK](#releasing-the-apk).

### Cancelling a share that is still arriving

A 512 MB attachment takes long enough that leaving is a real answer, so the
receiving screen offers `Cancel`. Two things make it work:

- The stager keeps a set of cancelled share ids and checks it between copy
  buffers, so a cancel costs at most one 64 KB read rather than the rest of the
  file.
- `discardShare` marks the cancellation on the calling thread, ahead of the
  bridge's single worker. A cancel that queued behind the staging it means to
  stop would arrive only once that staging had finished, and the share would
  come back as a ready prompt holding bytes nobody asked to keep.

### Real-device checks

Signed off on an SM-S901B running Android 16, with the 26 instrumentation tests
green on the same handset. Shares came from Samsung My Files (one file, and four
at once), the gallery, and a Chrome download. Also checked: cold and warm
launches, a duplicate filename, a 400 MB file, a 3 GB file, and a cancel during
ingestion.

- A duplicate is written as `name (1).ext` and the screen names what it became.
- A 400 MB file round-trips byte-identical. The app's native and Java heaps stay
  flat during it; the only growth is the Flutter renderer's graphics memory.
- A 3 GB file is refused before anything is staged, naming the 512 MB limit.
- A cancel mid-ingestion empties `cache/incoming` and returns the screen to its
  empty state, with no ready prompt appearing afterwards.

Low free space is the one case not exercised on hardware: filling a handset's
storage to force it is worth doing on a farm device, not a personal one.

### Farm provisioning

- Take the APK attached to the release, built by `mobile.yml`.
- Install it before a reservation begins so it belongs to the device baseline.
- Cover `/sdcard/Download/YARD Drop` in the device's `cleanup_paths`, which
  `/sdcard/Download` already does.
- Confirm the farm cleanup policy enables folder wiping.
- Verify that releasing a reservation removes every saved and staged public
  file while leaving the companion installed.

At the end of this increment, Android save-to-device is complete. Do not begin
iOS work.

Suggested commits:

```text
test(drop): cover Android share ingestion and storage
docs(drop): document Android provisioning and cleanup
```

## Increment 8: add the browser inbox destination

Expose `Send to YARD browser` in Flutter. It still writes through MediaStore,
which keeps the phone unaware of provider URLs and YARD credentials.

Use this layout:

```text
Download/YARD Drop/Inbox/
└── <timestamp>-<batch-id>/
    ├── batch.json
    ├── first-file.zip
    ├── second-file.pdf
    └── _YARD_READY
```

Rules:

- Generate a random batch ID for every share action.
- Publish files with `IS_PENDING` while they are incomplete.
- Write the versioned `batch.json` after every file publish has finished.
- Create `_YARD_READY` only after the manifest has been written.
- A reader ignores any batch without the marker.
- A failed batch remains in private staging for retry.
- Public inbox files remain until reservation cleanup so an interrupted browser
  download can be retried.

First validate this with the existing YARD Files dialog. This increment does
not change the provider, coordinator or web app.

### What a reader is promised

`_YARD_READY` is written last and is the whole handshake: a reader that sees it
is promised every file named in `batch.json`, complete. Nothing else about a
batch directory is stable enough to poll on, because MediaStore makes a row
visible as soon as `IS_PENDING` clears.

`batch.json` carries the schema version, the batch id, a creation timestamp, the
producing build, and one entry per file with the name MediaStore actually gave
it — a duplicate lands as `name (1).ext`, and the manifest has to name what is
on disk rather than what the sender sent. The build identity travels from Dart
so the manifest and the About screen can never disagree.

A batch is all-or-nothing. If any file fails, or the manifest or marker cannot
be written, the rows already published are deleted and the files go back to
pending. Their staged bytes are kept for exactly this, so a retry replays the
whole share into a fresh directory rather than repairing a half-written one.
This is the one place the save flow withdraws files it already wrote: for
Downloads a later failure never touches an earlier success, but half a batch is
worse than none, since nothing would ever read it and the bytes would sit in
Downloads until the reservation ended.

### Real-device checks

Signed off on the same SM-S901B, with 29 instrumentation tests and 41 Dart tests
green. A three-file share sent to the browser produced one directory holding the
files, `batch.json` and `_YARD_READY`, and the marker's leading underscore
survived MediaStore unmangled — the name it is given is the name on disk.

`The existing YARD Files dialog can download every item` is the one acceptance
criterion still open; it needs a provider running against this handset.

Acceptance criteria:

- `Send to YARD browser` writes a batch under Inbox.
- `batch.json` identifies the schema and producing application build.
- The ready marker never appears before its files are complete.
- The existing YARD Files dialog can download every item.
- A failed copy does not create a ready batch.

Suggested commit:

```text
feat(drop): write shared files to the YARD browser inbox
```

## Increment 9: show the inbox in the YARD web console

Add an Android-only `Receive shared files` action to the Files section of the
device console. It opens a new component:

```text
packages/web/src/components/device-drop-dialog.tsx
```

The dialog:

1. Tells the user to choose `Send to YARD` in the Android share sheet.
2. Polls `/sdcard/Download/YARD Drop/Inbox` while open.
3. Finds batch directories.
4. Ignores a batch without `_YARD_READY`.
5. Reads `batch.json` and rejects an unsupported schema with a useful error.
6. Lists completed files grouped by batch.
7. Downloads through the existing authenticated file endpoint.
8. Stops polling when closed.
9. Shows existing batches after a browser refresh.

Reuse `listDeviceFiles()`, `fetchDeviceFile()`, `formatBytes()` and the existing
token minting. The provider already lists and pulls arbitrary readable Android
paths. Its existing `file.pulled` event keeps these downloads in the audit log.

No database, coordinator or wire-protocol change is needed for this increment.

### Where the logic lives

The rule that decides what a browser may show is worth testing without a
browser, so it sits in `packages/web/src/lib/drop-inbox.ts` behind an
`InboxSource` of `list` and `read`. The dialog is the thin part: a React Query
poll whose `enabled` follows the dialog's open state, which is also all the
polling cleanup there is.

Two behaviours are deliberate and easy to read as bugs:

- **A listing failure on the inbox itself is an empty inbox.** Until somebody
  shares, the directory does not exist, and that is the state the dialog spends
  most of its life in. An error there would make the normal case look broken.
- **The manifest is checked against the listing.** A file the manifest names
  but the device no longer has is dropped rather than offered, because the
  download would fail at the point the user least expects it.

Batches sort newest first on the directory name. It starts with a UTC
timestamp, so ordering survives a manifest that could not be parsed.

**A manifest is read once per batch, ever.** Reading it is a file pull, and a
file pull writes an audit row — a dialog polling every two seconds would
otherwise fill the log with its own polling, one row per batch per tick. The
caller keeps the cache across polls; a batch is immutable once its marker is
there, so a second read would answer the same thing. A batch that stops being
listed, because cleanup removed it, is dropped from the cache with it.

`bun test packages/web/test/drop-inbox.test.ts` covers the empty inbox, an
incomplete batch, a missing and an unparseable manifest, an unsupported schema
naming the producing build, one and several batches, several files in a batch,
a batch removed mid-poll, and what repeated polls cost. The two React-level
cases in the list above — polling cleanup and hiding the control off Android —
are not covered: `packages/web` has no component test setup, and adding one is
its own decision.

### Real-device checks

Signed off against the SM-S901B with the coordinator, the Rust provider on the
`android` backend, and the batch increment 8 wrote. The reader found the one
complete batch and skipped four abandoned directories that never got a marker.
All three files downloaded byte-identical to the device's own `md5sum`, and each
pull is a `device.file_pull` row naming its inbox path. Five consecutive polls
cost exactly one manifest read.

Web tests cover:

- An empty inbox.
- An incomplete batch.
- A supported and an unsupported batch schema.
- One and several complete batches.
- Multiple files in one batch.
- Polling cleanup when the dialog closes.
- A download error.
- Hiding the control on non-Android devices.

Acceptance criteria:

- Open `Receive shared files` in a live Android session.
- Share the company ZIP to YARD Drop.
- Choose `Send to YARD browser`.
- See the ZIP appear without navigating the generic file browser.
- Download it and compare its bytes with the source.
- Confirm the existing audit log records the pull.

Suggested commit:

```text
feat(web): show files received through YARD Drop
```

## Increment 10: avoid browser memory for large files

The current web helper turns a complete response into a `Blob` before starting
the browser download. That is acceptable for the proof of concept but does not
fit arbitrary large attachments.

Add a download helper that:

1. Mints a fresh session token.
2. Builds the authenticated provider file URL.
3. Opens that URL through a temporary anchor.
4. Uses the provider's `Content-Disposition` filename.
5. Lets the browser stream the response to disk.

Use it for YARD Drop first. Decide separately whether the generic Files dialog
should adopt it.

`streamDeviceFile` in `packages/web/src/lib/screen/session.ts` mints a token,
builds the provider URL and hands it to the browser. The provider already
answered with `Content-Disposition: attachment` and a `Content-Length`, so
nothing changed on that side — the browser names the file and streams it to
disk, and the tab never holds it.

**A hidden iframe, not an anchor.** On the happy path either works, because an
`attachment` response downloads without navigating. But a provider that answers
an error sends no disposition, and an anchor would then navigate the whole tab
to it — throwing away a live session over a file that cleanup had already
removed. The iframe absorbs that.

The cost is that the browser owns the transfer from that point: no progress, no
completion, no error. The per-file spinner went with it. Minting the token is
the only part still worth reporting, and it is the only part that still can be.

The generic Files dialog still assembles a `Blob`. It browses a device rather
than collecting what somebody deliberately sent, and losing its error reporting
buys less there; that is a separate decision, deliberately not taken here.

`fetchDeviceFile` stays for `batch.json`, which is a few hundred bytes and is
parsed rather than saved.

## Releasing the APK

`.github/workflows/mobile.yml` builds the release APK and attaches it to the
GitHub release. It is reusable and called by `release.yml`, so the version comes
from release-please rather than from anything the build decides for itself, and
the same file gains the iOS job later.

The build stamps the released version, the CI run number and the short commit
through `--dart-define`. `BuildInfo` reads them, which puts them on the About
screen and in every `batch.json` the app writes, so a file pulled off a device
names the build that put it there.

The APK is signed with Android's debug key; there is no release keystore. An
install over a differently signed build is refused, so provisioning installs it
once as part of a device's baseline rather than upgrading in place.

`Mobile` does not run on a pull request. `Flutter` does — format, analyze,
`flutter test` and a debug build. The instrumentation suite needs a real device
and is run by hand:

```bash
cd apps/yard_drop/android && ./gradlew :app:connectedDebugAndroidTest
```

It uninstalls the app afterwards, taking its MediaStore-owned Downloads files
with it.

## Android definition of done

Android is complete when all of these statements hold:

- YARD Drop appears in the share sheet for arbitrary file attachments.
- Single and multiple file shares work.
- The app consumes temporary URI access before it can expire.
- File contents never cross a Flutter platform channel.
- `Save to Downloads` produces user-visible files.
- `Send to YARD browser` produces completed inbox batches.
- The YARD web console detects and downloads inbox files.
- Large files use bounded memory on the device, provider and browser.
- The companion requests no broad Android storage permission.
- The coordinator never carries or stores file bytes.
- No artifact database table exists.
- File pulls remain authenticated and audited.
- Reservation cleanup removes Saved and Inbox contents.
- Cleanup leaves the companion installed.
- Flutter CI runs separately from the Bun and Cargo jobs.
- The release identifies the app version, CI build number and source commit.
- Browser batches contain a supported schema version and producer version.
- The company ZIP passes an end-to-end byte comparison on a real farm device.

## iOS design

### Implementation notes

The Flutter 3.44.9 Runner and native SwiftUI ShareExtension require iOS 27.0.
Runner uses UIScene and registers plugins after its implicit Flutter engine
initializes. The extension never starts Flutter. CI builds simulator and
unsigned release targets and runs the native tests. Flutter workflows run only
when `apps/yard_drop/` changes, including release builds compared with the prior
tag. Users sign Runner and the extension with their own profiles and shared
App Group. See [the signing guide](../apps/yard_drop/ios/SIGNING.md).

The flow matches Android: share files, choose YARD - Device Farm, then Send to
YARD browser. Receive shared files discovers them automatically. Save on this
device uses the system Files picker. Neither action opens Runner or requires
Prepare or Finish controls in the browser.

The provider accesses the App Group through Apple's CoreDevice file service
on the existing USB/RSD tunnel. Production needs no Xcode or Python subprocess,
phone network connection or coordinator file route. The `idevice` dependency
provides RemoteXPC framing. Directory handles avoid the legacy recursive listing
callback; metadata operations are serialized, and file downloads stream in
256 KB chunks through the file data service. Connections end their file-service
session before closing. The configured group defaults to
`group.com.dinispimpao.yard.drop`; custom signing uses `options.drop_app_group`.
Both native Info.plists must carry that same value in `YardAppGroup`.

### Storage and reservation isolation

```text
Library/Caches/YARD Drop/
├── active-capture.json      # provider writes {id: reservation ID, expiresAt}
├── Exports/<staging-id>/    # extension owns incomplete copies and Files exports
└── Incoming/<reservation-id>/<batch-id>/
    ├── batch.json
    ├── diagnostics.zip
    └── _YARD_READY
```

The supervisor binds sharing before authorizing a reservation, then reconciles
against the current registry during revocation and its regular poll. This keeps
a delayed revocation from disarming a replacement reservation. The lease expires
after two minutes and refreshes every 30 seconds. The provider writes a temporary
record and renames it atomically. Replacing a reservation invalidates the old
record before deleting older inbox namespaces. Revocation invalidates it and
purges queued batches. Failed cleanup retries on subsequent polls.

The extension snapshots the ID before receiving attachments and rechecks the
current, unexpired ID before committing. Renewing the same ID while a file is
copying is valid. A changed ID rejects the share. The extension moves a complete
batch into Incoming atomically after writing its manifest and ready marker.
An old extension racing revocation can leave an old-ID directory, which is never
exposed to the next reservation and is removed on later cleanup.

The provider exposes the opaque `drop:/` root. Returned batch and file paths
include the current reservation ID, and stale IDs, traversal, symbolic links and
unexpected file types are refused. Existing file routes still require both a
valid JWT and the currently authorized reservation, and audit every pull. The
coordinator carries authorization and audit metadata only. Browser refreshes do
not reset the inbox.

The extension copies with a 64 KB buffer, accepts up to 20 files, 512 MB per file
and 2 GB per batch, and keeps successful attachments visible when others fail.
It sanitizes filenames, preserves extensions and disambiguates collisions before
queuing. The schema 1 manifest includes `captureId`, UTI and MIME type alongside
the Android-compatible file metadata. Payload names match their manifest display
names, so the system picker exports the same staged files without a second copy.

Browser batches use the reservation hook independently of optional farm folder
wiping. Files saved through the picker belong to the chosen location. Operators
can configure Runner's `/Documents/YARD Drop` under `cleanup_app_paths` to wipe
saved copies on release. Install the signed companion before the reservation
baseline so normal cleanup keeps it installed.

### Validation and rollout

On 2026-10-25, an iPhone 13 running iOS 27.0 passed release-mode Files sharing and
Files export using Xcode 27.2 and seven-day Personal Team profiles. Both profiles
needed the App Group registered and selected manually in Xcode. A 32 MB ZIP's
saved and browser copies matched source SHA-256
`0503896606b2d82c9954b9583cdbcb182393e22b5b9188a58552c10bf1c5f993`.
That first test used the earlier explicit launch handoff, which has since been
removed. The unsigned IPA re-sign procedure also passed signature verification.

On 2026-10-26, the same phone passed direct Rust CoreDevice App Group directory
listing, file creation, attributes, rename, streaming download with byte equality,
delete and session closure. The same probe passed reservation replacement, stale
path rejection, symlink rejection and release cleanup on that phone. The opt-in `app_group_files_work_over_the_usb_tunnel`
probe exercises these operations without launching the companion. Native tests
cover an in-flight lease refresh, reservation replacement, expiry, committed
bytes, names, copy limits, cancellation and mixed NSItemProvider results. Web
tests read iOS batches through the same inbox reader as Android. Rust tests check
reservation-bound paths, transfer size limits and delayed revocation. The revised
build passed 42 Flutter tests, 7 native iOS tests, 63 web tests, root type checking,
the Rust workspace tests and Clippy with warnings denied. Its signed release
build is installed for the remaining live browser share check.

Still to verify before wider rollout: the revised live browser flow, Photos,
Safari, the company ZIP, multiple attachments on a physical device, low storage,
extension termination, signing upgrades, provider reconnects and reservation
release. Check downloaded bytes and audit entries. Hardware tests are opt-in
with `YARD_TEST_IOS_UDID`; normal Cargo checks require no phone.

Relevant references:

- [Sharing app and extension data](https://developer.apple.com/documentation/technologyoverviews/shared-data)
- [NSItemProvider file representations](https://developer.apple.com/documentation/foundation/nsitemprovider/loadfilerepresentation(fortypeidentifier:completionhandler:))
- [CoreDevice file-service protocol reference](https://github.com/doronz88/pymobiledevice3/blob/master/pymobiledevice3/remote/core_device/file_service.py)

## Work postponed beyond both mobile implementations

- Direct phone-to-provider HTTP uploads.
- Durable artifact storage or an artifact database table.
- Provider-managed companion installation and upgrades.
- Android 9 and older storage support.
- Automatic App Store or Play Store distribution.
