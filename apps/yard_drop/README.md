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
| iOS browser lease | 2 minutes, renewed by the provider every 30 seconds |

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

During a reservation, share files to YARD - Device Farm and select Send to YARD
browser. They appear in Receive shared files automatically, just like Android.
The provider writes a short lease into the App Group when the reservation starts,
renews it while connected, and reads completed batches directly through Apple's
CoreDevice file service over the existing USB tunnel. Runner does not open during
this flow. Browser refreshes preserve the inbox.

The extension rechecks the reservation ID before committing each batch. The
provider exposes only the current reservation's files at the opaque `drop:/`
root. File tokens must still pass the provider's current-reservation check.
Revocation invalidates the lease and removes queued batches. If the provider
loses the phone, sending disables after the two-minute lease expires.

The iOS target comes from Flutter 3.44.9 and uses Swift Package Manager and
`FlutterSceneDelegate`. Plugin registration happens in
`didInitializeImplicitFlutterEngine`, after the implicit engine exists. See Flutter's
[UIScene migration](https://docs.flutter.dev/release/breaking-changes/uiscenedelegate).

Native tests run on macOS with a full Xcode installation:

```bash
xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner \
  -destination 'platform=iOS Simulator,name=<available iPhone>' CODE_SIGNING_ALLOWED=NO
```

These cover reservation replacement and lease refresh during a share, expiry,
byte comparisons, filename collisions, limits, cancellation, and mixed successful
and failed `NSItemProvider` results. Provider tests cover path isolation and the
USB file service. Physical-device tests are opt-in with `YARD_TEST_IOS_UDID`.

### Signing and release

GitHub releases publish `yard-drop-<version>+<build>-ios-unsigned.ipa` and a
SHA-256 checksum. Building and publishing this archive needs no Apple account,
certificate, provisioning profile or signing secret. An unsigned IPA cannot be
installed on an ordinary iPhone until its user signs both the app and extension.

Use `python3 ios/sign-release.py --help` to sign a local unsigned IPA
with your team, custom bundle IDs and App Group using Xcode's signed-in account
and Release Testing export flow. Pass `--export-options` with the local
`ExportOptions.plist` from a successful Xcode export to reuse its signing settings.
No manual certificate import is required when cloud signing is available.
See [iOS signing instructions](ios/SIGNING.md) for company account setup and
the separate source-build route for Xcode's free seven-day Personal Team provisioning.
Both targets must share an App Group authorized by their profiles. Source builds
generate `YardAppGroup` in each target's Info.plist from the single group selected
in Signing & Capabilities. The IPA signing script updates it when re-signing.
Set the provider device option
`drop_app_group` to that same value when using a custom group.

CI stamps the root version, `run_number * 100 + run_attempt`, and source commit
into both targets. `ios/Flutter/BuildInfo.xcconfig` defaults the native source
commit to `local`; CI replaces it before building. Android and iOS CI run for
changes under `apps/yard_drop/` or to `.github/workflows/flutter.yml`. The mobile
release workflow builds both platforms on every release, so each release has
an APK and an unsigned IPA even when the app source has not changed.

For PR test builds, open the Flutter workflow run from the PR's Checks tab.
Under Artifacts, download `app-debug.apk` or `yard-drop-ios-unsigned.ipa`
directly. The Android build is a debug APK; the iOS build is a release IPA that
needs signing using
[the signing guide](ios/SIGNING.md) before installation.

Install the signed IPA before the reservation baseline. For a custom App Group:

```yaml
devices:
  - udid: <ios-udid>
    backend: ios
    options:
      drop_app_group: group.com.example.yarddrop
```

Browser batches are purged by the reservation hook, independently of optional
farm folder cleanup. Files saved through the system picker belong to the chosen
location. If saving inside Runner Documents, use `cleanup_app_paths` with the
actual signed bundle ID and `/Documents/YARD Drop`, and enable `cleanup.enabled`
and `cleanup.wipeFolders` to remove those saved copies on release.

Before rollout, test Files, Photos, Safari and the company app, multiple files,
picker cancellation, extension termination, low storage, expired leases, provider
reconnects, release, and upgrades. Compare downloaded bytes and inspect the audit
log. Physical-device evidence is recorded in
[docs/DROP.md](../../docs/DROP.md#implementation-notes).

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
