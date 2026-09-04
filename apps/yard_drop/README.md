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
