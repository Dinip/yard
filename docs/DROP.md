# YARD Drop implementation plan

> Status: Android save-to-device, the browser inbox and its web dialog are done
> and signed off on hardware (increments 0 through 9). Everything iOS is still
> a plan.

YARD Drop is a first-party Flutter companion that appears as an Android share
target. It accepts file attachments from any app and gives the tester two
choices:

- **Save on this device** copies the files to a user-visible Downloads folder.
- **Send to YARD browser** puts the files in a short-lived device inbox that the
  current YARD session can download.

Android comes first. The iOS application and Share Extension are designed below,
but their implementation does not begin until the Android flow has passed the
end-to-end tests in this document.

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

Only publish the Android files until iOS is implemented. Call the reusable
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

Instrumentation tests live in `apps/yard_drop/android/app/src/androidTest`, and
CI runs them on API 29 and API 35 under the emulator.

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

- Build an internally signed APK.
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

Acceptance criteria:

- The browser does not construct a `Blob` for a YARD Drop download.
- The provider still stages, hashes, streams and removes its temporary copy.
- An aborted download leaves the device inbox copy available for retry.
- The audit event still lands before the response body begins.

Suggested commit:

```text
perf(web): stream YARD Drop downloads to disk
```

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

iOS keeps the Flutter containing app but needs a native Share Extension target.
The extension runs in a separate sandbox and may run while the containing app is
stopped. Both targets use an App Group to exchange staged files and small JSON
manifests.

The iOS design does not embed a Flutter engine in the Share Extension. Its UI is
only a file summary and two actions, so a small SwiftUI view hosted by the
extension is cheaper to start, easier to test on physical devices and less
likely to hit an extension memory limit. The containing application and its
workflow remain Flutter.

The iOS flow is:

```text
iOS share sheet
      │
      ▼
native Share Extension
      │
      ├── Save on this device
      │       └── system document picker
      │
      └── Send to YARD browser
              └── App Group staging
                        │
                        ▼
              Flutter containing app
                        │
                        └── Documents/YARD Drop/Inbox
                                      │
                                      ▼
                          existing House Arrest pull
                                      │
                                      ▼
                              browser download
```

The Share Extension cannot write directly into the containing app's Documents
directory. It copies attachments into the App Group first. The containing app
later publishes a completed batch into its own Documents directory, which the
provider can read through House Arrest.

Do not rely on a Share Extension opening its containing app automatically.
Apple decides which extension points may open URLs, and Share Extensions are not
documented as supporting that handoff. The browser flow below launches the
containing app through YARD's existing device app-launch command instead.

Relevant Apple references for implementation:

- [Sharing data between an app and its extension](https://developer.apple.com/documentation/technologyoverviews/shared-data)
- [`NSItemProvider.loadFileRepresentation`](https://developer.apple.com/documentation/foundation/nsitemprovider/loadfilerepresentation(fortypeidentifier:completionhandler:))
- [`NSExtensionActivationSupportsFileWithMaxCount`](https://developer.apple.com/documentation/bundleresources/information-property-list/nsextension/nsextensionattributes/nsextensionactivationrule/nsextensionactivationsupportsfilewithmaxcount)
- [`NSExtensionContext.open`](https://developer.apple.com/documentation/foundation/nsextensioncontext/open(_:completionhandler:))

### iOS storage layout

The App Group contains private transfer state:

```text
Library/Caches/YARD Drop/
├── active-capture.json
└── Incoming/
    └── <capture-id>/<batch-id>/
        ├── batch.json
        ├── first-file.zip
        └── _YARD_READY
```

The containing application's file-sharing directory contains batches that the
provider may pull:

```text
Documents/YARD Drop/Inbox/
└── <capture-id>/<batch-id>/
    ├── batch.json
    ├── first-file.zip
    └── _YARD_READY
```

The Runner target sets `UIFileSharingEnabled` so it appears in YARD's synthetic
iOS file root. YARD addresses the inbox as:

```text
app:<companion-bundle-id>:/Documents/YARD Drop/Inbox
```

### Shared batch manifest

Swift and Dart exchange a versioned JSON file rather than private plugin types:

```json
{
  "schemaVersion": 1,
  "batchId": "random-id",
  "captureId": "random-id-or-null",
  "createdAt": 1788364800000,
  "producer": {
    "appVersion": "0.4.0",
    "buildNumber": 142,
    "commit": "abc1234"
  },
  "files": [
    {
      "id": "random-id",
      "name": "diagnostics.zip",
      "typeIdentifier": "public.zip-archive",
      "size": 48291,
      "stagedName": "random-id.bin"
    }
  ]
}
```

The extension writes file data first, then `batch.json`, then `_YARD_READY`.
Readers ignore a directory without the marker. The format version allows a later
application to reject or migrate a batch written by an older extension.

### Reservation binding on iOS

App Group data is invisible to the provider and therefore cannot rely only on
provider cleanup. A stale batch from one reservation must never be published by
the next user.

The browser controls an explicit two-launch handoff:

1. The user opens `Receive shared files` in YARD.
2. The browser creates a random capture ID.
3. YARD launches the containing app with an `arm` argument and that capture ID.
4. The containing app purges an older App Group capture and records the new one.
5. The user returns to the tested app and shares files to YARD Drop.
6. The Share Extension enables `Send to YARD browser` only while that capture is
   present and unexpired.
7. The extension stages the batch under that capture ID.
8. The user returns to YARD and selects `Finish transfer`.
9. YARD launches the containing app with a `publish` argument and the same ID.
10. The containing app moves only matching ready batches into Documents.
11. The web dialog detects those batches and offers the downloads.

Opening the containing app twice is less elegant than Android, but it uses
documented application launching, binds the files to a browser-created secret
and keeps bytes off the coordinator. A future transport may shorten the flow,
but it must preserve those properties.

The containing app never displays or publishes an App Group batch during an
ordinary launcher start. Only a matching, unexpired `publish` argument may move
files into Documents.

## iOS Increment 0: validate the platform handoff

Before adding permanent iOS source, run a focused experiment on one farm iPhone:

- Confirm the provider's existing launch command delivers arguments to an iOS
  application.
- Confirm a Runner application with `UIFileSharingEnabled` appears in YARD's
  synthetic file root.
- Confirm House Arrest can list and pull nested files from its Documents
  directory.
- Confirm a Share Extension can copy a representative company ZIP into an App
  Group without loading it into memory.
- Confirm the system document picker can be presented from the extension.

If launch arguments do not arrive reliably, stop before building the browser
handoff. Replace that control mechanism on paper first. Do not fall back to an
undocumented attempt to open the containing app from the extension.

Acceptance criteria:

- Each behavior above has a small reproducible probe or written test result.
- The company ZIP survives an extension to App Group byte comparison.
- No production UI or protocol is committed around an unverified assumption.

Suggested commit:

```text
test(drop): probe the iOS share and launch handoff
```

## iOS Increment 1: add the Runner and Share Extension targets

After Android is complete, add iOS to the existing Flutter project:

```bash
cd apps/yard_drop
flutter create --platforms=ios .
```

Configure:

- A permanent Runner bundle ID.
- A Share Extension bundle ID beneath it.
- One App Group entitlement shared by Runner and the extension.
- The Apple development team and provisioning profiles.
- The minimum iOS version supported by the farm inventory.
- `UIFileSharingEnabled` on Runner.
- `LSSupportsOpeningDocumentsInPlace` if the document-picker behavior requires
  it after the Increment 0 probe.

Add the Share Extension with `NSExtensionPointIdentifier` set to
`com.apple.share-services`. Its activation rule supports files up to the batch
count chosen for Android. Do not enable plain text or web URLs.

Add a native `ShareViewController` that hosts a small SwiftUI screen. Do not add
third-party receiving packages or a Flutter engine to the extension.

Acceptance criteria:

- The containing Flutter app builds and runs on an iPhone.
- The extension is embedded and signed with the containing app.
- `Send to YARD` appears for a generic file attachment.
- It does not appear for a plain text-only share.
- The normal Flutter application still works without invoking the extension.

Suggested commit:

```text
feat(drop): add the iOS Share Extension target
```

## iOS Increment 2: receive and stage one file

The Share Extension reads `NSExtensionItem.attachments`. For each
`NSItemProvider`:

1. Find a registered type that conforms to file data.
2. Ask for a file representation.
3. Read its size and suggested name without loading its contents into `Data`.
4. Enforce the same count and size policy used on Android.
5. Copy the temporary representation into the App Group before the provider's
   completion handler returns.
6. Sanitize the display name.
7. Write `batch.json`.
8. Write `_YARD_READY` last.
9. Call `completeRequest` only after the chosen operation finishes or the batch
   is safely staged.

The system may delete the URL returned by `loadFileRepresentation` when its
completion handler returns. Copying within that handler is required.

The SwiftUI screen shows the same concepts as Flutter:

- Attachment names and sizes.
- `Save on this device`.
- `Send to YARD browser`.
- Progress, cancellation and an actionable error.

The browser action is disabled with `Open Receive shared files in YARD first`
when no valid capture is armed.

Acceptance criteria:

- A single company ZIP reaches the App Group unchanged.
- Missing names, unknown types and unavailable representations fail clearly.
- Cancelling removes partial files.
- Extension memory remains bounded for a large file.

Suggested commit:

```text
feat(drop): stage incoming iOS file shares
```

## iOS Increment 3: save through the Files picker

`Save on this device` presents a `UIDocumentPickerViewController` for exporting
a copy of the staged file. The system lets the user choose On My iPhone, iCloud
Drive or another configured file provider.

For a batch, export all staged URLs in one picker when the selected iOS version
supports the expected multi-file experience. If it does not, present one clear
save step per file and keep the remaining files staged.

After a successful export:

- Complete the extension request.
- Remove the App Group staging copy.
- Leave the user-selected copy untouched.

After cancellation:

- Keep the extension open for another destination choice when the system allows
  it.
- Otherwise remove the staged batch when the extension closes.

Acceptance criteria:

- The company ZIP can be saved under On My iPhone.
- The saved bytes match the source.
- Cancellation leaves no unexpected copy.
- Multiple files have explicit per-file results.

Suggested commit:

```text
feat(drop): save iOS shares through the Files picker
```

## iOS Increment 4: support multiple and mixed attachments

Extend the staging code to every `NSExtensionItem` and `NSItemProvider` in the
request. Preserve stable ordering and deduplicate representations of the same
attachment where possible.

Handle:

- Several extension items.
- Several attachments on one item.
- Duplicate suggested filenames.
- Mixed Uniform Type Identifiers.
- A provider that offers more than one representation.
- One failed provider among successful providers.
- Extension cancellation during asynchronous loading.

Keep the manifest format compatible with the Dart model. Store the native type
identifier as metadata, but treat its bytes as opaque.

Acceptance criteria:

- Single and multiple shares pass on a simulator and real iPhone.
- Mixed file types remain one batch.
- One failure does not hide successful items.
- No temporary provider URL is retained after its callback.

Suggested commit:

```text
feat(drop): support multiple iOS share attachments
```

## iOS Increment 5: arm and publish a browser capture

Add an iOS Runner platform bridge beside the Android `ShareGateway`
implementation. It handles launch arguments and App Group operations without
passing file bytes through Dart.

Arm behavior:

- Receive a browser-generated capture ID and expiry through the launch command.
- Purge any older App Group capture before storing the new one.
- Persist `active-capture.json` atomically.
- Show a Flutter confirmation that YARD Drop is ready.

Publish behavior:

- Require the same capture ID and a non-expired arm record.
- Ignore and purge batches for any other ID.
- Copy matching ready batches into
  `Documents/YARD Drop/Inbox/<capture-id>/`.
- Write `_YARD_READY` last in Documents.
- Delete the App Group source only after the Documents copy completes.
- Clear the arm record after publishing.

An ordinary app launch does neither operation and does not reveal pending App
Group batches.

Acceptance criteria:

- An unarmed extension cannot queue a browser delivery.
- A wrong or expired capture ID cannot publish files.
- A previous capture is purged when a new one is armed.
- A matching capture appears in Runner Documents unchanged.
- Restarting the Runner midway does not produce a ready partial batch.

Suggested commit:

```text
feat(drop): publish an armed iOS browser capture
```

## iOS Increment 6: make app Documents cleanup-safe

The current iOS cleanup implementation wipes only the AFC media domain. YARD
Drop's browser inbox lives in an app Documents container, so provider cleanup
must gain a narrowly configured app-container operation.

Add structured provider configuration rather than putting a synthetic app path
into the existing absolute media path list:

```yaml
devices:
  - udid: <ios-udid>
    backend: ios
    cleanup_app_paths:
      - bundle_id: <companion-bundle-id>
        path: /Documents/YARD Drop
```

Validation must:

- Require a non-empty bundle ID.
- Require a path beneath `/Documents`.
- Refuse `/Documents` itself.
- Refuse `..`, empty components and paths outside the app Documents tree.
- Keep the configuration provider-local because it ends in recursive deletion
  on a device.

The iOS backend uses House Arrest `VendDocuments` for the configured bundle and
removes only the validated relative subtree. The cleanup report writes a
readable synthetic path such as:

```text
app:<bundle-id>:/Documents/YARD Drop
```

Acceptance criteria:

- Release cleanup removes every published inbox batch.
- Cleanup leaves the containing app installed.
- A rejected configuration fails provider startup before touching a device.
- Android cleanup behavior remains unchanged.
- Cleanup errors appear in the existing `device.cleanup` audit entry.

Suggested commit:

```text
feat(provider): clean configured iOS app documents
```

## iOS Increment 7: connect the web inbox flow

Generalize `device-drop-dialog.tsx` so the provider supplies or the web client
selects the correct inbox root:

```text
Android: /sdcard/Download/YARD Drop/Inbox
iOS:     app:<companion-bundle-id>:/Documents/YARD Drop/Inbox
```

The iOS dialog has explicit stages:

1. `Prepare iPhone` launches Runner with a new `arm` capture ID.
2. The UI tells the user to return to the tested app and share to YARD Drop.
3. `Finish transfer` launches Runner with `publish` and the same ID.
4. The dialog polls the iOS app Documents inbox for that capture ID.
5. Ready batches appear with the same download UI used by Android.

Do not send file bytes or manifests through tRPC. App launch remains a control
operation. Browsing and downloads remain direct browser to provider requests.

Acceptance criteria:

- The full arm, share, publish and download sequence works from a YARD session.
- A browser refresh can resume a capture whose ID remains in session UI state or
  explicitly restart it without publishing stale data.
- A revoked reservation cannot keep polling or downloading.
- The existing file-pull audit event records the final download.

Suggested commit:

```text
feat(web): receive YARD Drop files from iOS
```

## iOS Increment 8: tests, signing and farm rollout

Add:

- Swift unit tests for type selection, filename sanitizing and batch manifests.
- Extension integration tests for one and many `NSItemProvider` attachments.
- Dart tests for arm and publish launch states.
- Provider tests for structured app cleanup path validation.
- Web tests for the iOS dialog stages and inbox path.
- A release-mode physical-device test because extension behavior and memory
  differ from the simulator.

Manual source applications include Files, Photos, Safari downloads and the
company application. Test extension termination, low space, a provider restart,
an expired capture and reservation release before publish.

Distribution work includes:

- Runner and extension provisioning profiles.
- App Group entitlement provisioning.
- Internal IPA signing.
- Installing the companion before the reservation baseline.
- Recording the companion bundle ID in provider cleanup configuration.
- An iOS build in the reusable mobile workflow called by the root release. It
  must not become part of the container image job.

Suggested commits:

```text
test(drop): cover the iOS share and browser flow
docs(drop): document iOS signing and farm rollout
```

## iOS definition of done

iOS is complete when:

- YARD Drop appears for generic file attachments in the share sheet.
- The extension handles single, multiple and mixed file types.
- `Save on this device` exports through the Files picker.
- Browser delivery requires a matching, unexpired capture ID.
- The extension and Runner exchange only files and versioned manifests through
  their App Group.
- The provider pulls published files from Runner Documents through House Arrest.
- File bytes never pass through the coordinator or a Flutter platform channel.
- A reservation release removes published Documents files.
- A new arm operation removes stale private App Group captures.
- The extension uses bounded memory and passes release-mode testing on a real
  farm iPhone.
- Runner, Share Extension and App Group signing survive an install and upgrade.
- Android and iOS builds share the root YARD semantic version.
- Android behavior and tests remain unchanged.

## Work postponed beyond both mobile implementations

- A private USB protocol between either companion and the provider.
- Direct phone-to-provider HTTP uploads.
- Durable artifact storage or an artifact database table.
- Provider-managed companion installation and upgrades.
- Android 9 and older storage support.
- Automatic App Store or Play Store distribution.
