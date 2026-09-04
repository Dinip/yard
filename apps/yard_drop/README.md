# YARD Drop

The Android share companion for `YARD - Device Farm`. A user shares a file from
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
| Minimum Android API | 29 (Android 10) |
| Share sheet label | `YARD - Device Farm` |
| Files per share | 20 |
| Size per file | 512 MB |
| Size per batch | 2 GB |
| Abandoned staging lifetime | 24 hours |

API 29 is the floor because everything above it gets scoped storage, so the
save path is MediaStore only, with no legacy external-storage fallback.

## Commands

```bash
flutter pub get
flutter analyze
flutter test
flutter run              # a connected Android device or emulator
flutter build apk --debug
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

CI runs them on API 29 and API 35. One test stages a 64 MB share to prove the
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
