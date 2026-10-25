# YARD Drop

The Android and iOS share companion for `YARD - Device Farm`. A user shares a file from
any app on a farm device, picks `YARD - Device Farm` in the share sheet, and
then chooses where it goes: saved on the device, or handed to the browser they
reserved the device from.

The full design and increment plan lives in [docs/DROP.md](../../docs/DROP.md).

## Decisions

These are fixed. The application ID in particular cannot change after the first
release without producing a different app on every device.

| Decision | Value |
|---|---|
| Application ID | `com.dinispimpao.yard.drop` |
| iOS extension bundle ID | `com.dinispimpao.yard.drop.ShareExtension` |
| iOS App Group | `group.com.dinispimpao.yard.drop` |
| Minimum iOS deployment target | 27.0 |
| Minimum Android API | 29 (Android 10) |
| Share sheet label | `YARD - Device Farm` |
| Files per share | 20 |
| Size per file | 512 MB |
| Size per batch | 2 GB |
| Abandoned staging lifetime | 24 hours |
| iOS browser capture lifetime | 10 minutes |

API 29 is the floor because everything above it gets scoped storage, so the
save path is MediaStore only, with no legacy external-storage fallback.

## Commands

```bash
flutter pub get
flutter analyze
flutter test
flutter run              # a connected Android device or emulator
flutter build apk --debug
flutter build ios --simulator --debug --build-number=1
```

The share receiver itself is only true on a device, so it is covered by
instrumentation tests rather than Dart ones. A fake `ContentProvider` in the
test APK serves the streams a real sender produces, including the awkward ones:
no display name, no size, a stream that dies partway, a URI the app may not
read.

```bash
flutter build apk --debug        # the Gradle test task needs Flutter's assets
cd android && ./gradlew :app:connectedDebugAndroidTest
```

Run them on API 29 and API 35. One test stages a 64 MB share to prove the
copy loop never holds a file in memory, so an emulator needs room for it.

The version comes from the root release: release-please writes the same
semantic version into `pubspec.yaml` that it writes to the TypeScript packages
and the Cargo workspace. CI supplies the build number and commit:

```bash
flutter build apk \
  --build-name 0.4.0 --build-number 142 \
  --dart-define YARD_VERSION=0.4.0 \
  --dart-define YARD_BUILD_NUMBER=142 \
  --dart-define YARD_COMMIT=abc1234
```

A local build shows `dev+0 (local)` on the About screen instead.

## iOS

The Share Extension is native SwiftUI and does not start a Flutter engine.
It copies a file representation before its provider callback returns, with a
64 KB buffer and the same count and byte limits as Android. Failed attachments
remain visible alongside successful files. Save on this device exports all
successful files through the system Files picker. Cancelling the picker returns
to the destination choices.

Browser delivery takes two launches because a Share Extension cannot reliably
open its containing app:

1. Open Receive shared files in YARD and select Prepare iPhone.
2. Wait for Ready to receive on the iPhone. Return to the app under test.
3. Share files to YARD - Device Farm and select Send to YARD browser.
4. Select Finish transfer in YARD, then download the files as they appear.

Preparing creates a new random capture ID and removes the previous capture's
queued and published files. The extension captures that ID before receiving
attachments and rechecks it when sending. Publishing requires the same ID and
an unexpired arm record. An ordinary launcher start never publishes or lists
private shares. After refreshing the browser, prepare a new transfer.

Published files live under
`app:com.dinispimpao.yard.drop:/Documents/YARD Drop/Inbox/<capture-id>`.
The provider reads them through its existing authenticated House Arrest file
plane. The coordinator carries app-launch commands and audit metadata only.

The iOS target comes from Flutter 3.44.9 and uses Swift Package Manager and
`FlutterSceneDelegate`. Platform-channel registration happens in
`didInitializeImplicitFlutterEngine`, rather than reading an AppDelegate window
before the scene exists. See Flutter's
[UIScene migration](https://docs.flutter.dev/release/breaking-changes/uiscenedelegate).

Native tests run on macOS with a full Xcode installation:

```bash
xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner \
  -destination 'platform=iOS Simulator,name=<available iPhone>' CODE_SIGNING_ALLOWED=NO
python3 ios/test_handoff.py <simulator-udid>  # uses a disposable simulator
```

These cover capture expiry and replacement, launch-command validation,
publication byte comparisons, incomplete copies, filename collisions, byte
limits, cancellation, and mixed successful and failed `NSItemProvider` results.
Flutter tests cover the containing app's launch states. A simulator build does
not replace the physical-device checks below.

### Signing and release

GitHub releases publish `yard-drop-<version>+<build>-ios-unsigned.ipa` and a
SHA-256 checksum. Building and publishing this archive needs no Apple account,
certificate, provisioning profile or signing secret. An unsigned IPA cannot be
installed on an ordinary iPhone until its user signs both the app and extension.

See [iOS signing instructions](ios/SIGNING.md) for signing a downloaded IPA with
your own profiles, or using Xcode's free seven-day Personal Team provisioning.
Both targets must share an App Group authorized by their profiles. The native
App Group identifier is read from `YardAppGroup` in each target's Info.plist, so
users can change it when signing. The browser discovers a renamed bundle ID by
the unchanged `YARD - Device Farm` display name or `yard_drop` bundle name.

CI stamps the root version, `run_number * 100 + run_attempt`, and source commit
into both targets. `ios/Flutter/BuildInfo.xcconfig` defaults the native source
commit to `local`; CI replaces it before building. Android and iOS CI run only
for changes under `apps/yard_drop/`. The mobile release workflow compares that
folder with the previous release before starting either Flutter build.

Install the signed IPA before the reservation baseline. Configure its published
Documents subtree on each iPhone:

```yaml
devices:
  - udid: <ios-udid>
    backend: ios
    cleanup_app_paths:
      - bundle_id: com.dinispimpao.yard.drop  # use the actual signed bundle ID
        path: /Documents/YARD Drop
```

Enable `cleanup.enabled` and `cleanup.wipeFolders` in the farm settings. The
provider cannot see the private App Group. A new capture purges stale queued
batches, and mismatched or expired captures cannot publish them.

Before rolling out to the farm, use a release build on a provisioned iPhone to
check Files, Photos, Safari and the company app. Compare the company ZIP's
bytes after App Group staging and browser download; test picker cancellation,
extension termination, low storage, expired captures and release before
publishing. Confirm House Arrest can pull nested Documents files, release
cleanup removes them, and the app and extension survive an upgrade. These
broader hardware checks remain rollout gates. Single-file sharing, Files export,
capture publication and seven-day Personal Team signing passed on an iPhone 13
with iOS 27.0. See the recorded results in [docs/DROP.md](../../docs/DROP.md#implementation-notes).

## Farm provisioning

YARD Drop belongs to a device's baseline, not to a session. Install it before a
reservation begins:

```bash
adb install -r build/app/outputs/flutter-apk/app-release.apk
```

Installing it during a session makes it an app the session installed, and
cleanup uninstalls it on release along with everything else the user added.

Saved files go to `Download/YARD Drop/Saved`, which is user-visible and outlives
a reservation on its own. Add it to the device's `cleanup_paths` in the
provider's YAML so a release wipes it, unless the device already wipes all of
`/sdcard/Download`:

```yaml
devices:
  - udid: R5CT30XXXXX
    backend: android
    cleanup_paths:
      - /sdcard/Download/YARD Drop
```

Wiping folders is off by default farm-wide, so also enable `cleanup.wipeFolders`
in `/admin/settings`. Without it the path is configured and never used. See
[docs/CLEANUP.md](../../docs/CLEANUP.md).

Staged files never reach that folder. They live in the app's private cache and
are removed when a share is answered for, or 24 hours later at the latest, so a
reservation ending mid-share leaves nothing readable to the next user.

After provisioning a device, confirm on it that:

- The share sheet offers `YARD - Device Farm` from Files, Photos and a browser
  download.
- A saved file appears under `Download/YARD Drop/Saved` in the Files app.
- Releasing the reservation empties that folder and leaves the app installed.
