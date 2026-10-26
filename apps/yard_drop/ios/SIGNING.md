# Signing YARD Drop for an iPhone

YARD releases contain an unsigned, device-architecture IPA. Anyone can build
and publish that archive without an Apple certificate. The farm operator signs
it with their own Apple account before installation. Nothing is uploaded to
Apple by the YARD release workflow, and no signing credentials belong in GitHub.

An IPA is a ZIP containing `Payload/Runner.app`. Renaming that ZIP to `.ipa`
does not sign it. Both `Runner.app` and its embedded
`PlugIns/ShareExtension.appex` need valid signatures and provisioning profiles.
The Flutter frameworks need signatures too.

## Using Xcode and a free Personal Team

Use this route for a personal test device. Apple provisions Personal Team apps
for seven days, after which they must be signed and installed again. Apple also
limits Personal Teams to three devices, three installed apps per device, and
ten short-lived App IDs. Runner and ShareExtension use separate App IDs. See
[Apple's account overview](https://developer.apple.com/help/account/basics/about-your-developer-account/).

1. Install Xcode 27 or newer and Flutter 3.44.9 on a Mac. The iPhone must run
   iOS 27 or newer. In Xcode Settings, add your
   Apple account. Enter its password in Xcode, never in a terminal script or
   GitHub secret for this project.
2. Connect the iPhone over USB, trust the Mac and enable Developer Mode on the
   iPhone. Keep it unlocked during the first installation.
3. Check out the YARD release tag, run `flutter pub get` in `apps/yard_drop`,
   then open `ios/Runner.xcworkspace`.
4. Select Runner, then Signing & Capabilities. Enable automatic signing and
   choose your Personal Team. Do the same for ShareExtension.
5. Keep the default bundle IDs if Xcode can register them for your team. If an
   identifier belongs to another team, choose your own Runner ID and an extension
   ID below it, for example `com.example.yarddrop` and
   `com.example.yarddrop.ShareExtension`.
6. Both targets need the same registered App Group. Add it in Signing &
   Capabilities and select it on both targets. If changing the default group,
   update `YardAppGroup` in **both** targets' Info.plist to that same identifier.
   Set the provider device option `drop_app_group` to the same identifier.
7. Choose the connected iPhone and use the Runner scheme. Set its Run build
   configuration to Release, then run it. Flutter's containing app must work
   without a debugger when opened from the share workflow.

Check that both provisioning profiles actually contain the group under
`com.apple.security.application-groups`. An empty group list cannot authorize
the shared container. Do not remove that entitlement to get past a signing
error; the extension and containing app would lose their transfer channel.
Apple's [capability matrix](https://developer.apple.com/help/account/reference/supported-capabilities-ios)
and [App Group setup](https://developer.apple.com/documentation/xcode/configuring-app-groups)
describe account support and registration. A team that cannot provision the
required group cannot run this implementation merely by signing an IPA.

This source-build route lets Xcode create the development certificate and
profiles. No paid distribution certificate is needed for a seven-day device
test. It does not produce a universally installable IPA.

## Signing a downloaded unsigned IPA

This script follows Xcode's **Archive > Distribute App > Release Testing**
workflow, also called Ad Hoc distribution. Use it when you already sign in to
Xcode with your company Apple account and your admin has registered the two
bundle IDs and their shared App Group.

You do not need to import or export a certificate manually. Xcode handles
signing through your account, including cloud-managed distribution certificates
when your team grants access. See Apple's
[cloud signing guide](https://developer.apple.com/videos/play/wwdc2021/10204/).
This export route requires a paid team with Ad Hoc distribution access. For a
free Personal Team, use the source-build instructions above.

1. Use a Mac with Xcode 27 or newer, Python 3.9 or newer and the GitHub CLI.
   No Flutter installation or source build is needed.
2. In **Xcode > Settings > Apple Accounts**, sign in with the account you
   normally use to export IPAs and check that the intended team appears.
3. Ask your admin for the Runner bundle ID, extension bundle ID, shared App
   Group and ten-character Team ID. Both app IDs must have that group enabled,
   and the destination iPhones must be registered for Ad Hoc distribution.
   Use the same account permissions as your normal Release Testing export.
4. Sign in with `gh auth login` if GitHub requires authentication for the repo.
5. From `apps/yard_drop`, run:

```bash
python3 ios/sign-release.py \
  --team ABCDE12345 \
  --bundle-id com.example.yarddrop \
  --extension-bundle-id com.example.yarddrop.ShareExtension \
  --app-group group.com.example.yarddrop \
  --output yard-drop-signed.ipa
```

Use the Team ID, not your account email. The extension ID must start with the
Runner ID followed by a dot. The script does not ask for an Apple password or
an API key. If Xcode's account session has expired, sign in again in Xcode.
If export fails on permissions or profiles, resolve that with your admin just
as you would for an export in Xcode.

The script pins the latest GitHub release, downloads its unsigned IPA and
SHA-256 checksum, and verifies the checksum. It updates both bundle IDs and
`YardAppGroup` values in a temporary directory, then prepares an Xcode archive.
Temporary ad hoc signatures carry the App Group entitlement into Xcode's
export step on both Runner and the extension. These temporary signatures use
no Apple certificate and are replaced during export.

The script runs `xcodebuild -exportArchive` with automatic signing, your team,
`method: release-testing` and `destination: export`. It uses Xcode's existing
account login. `-allowProvisioningUpdates` permits Xcode to fetch or refresh
signing assets, including creating managed profiles or certificates if required
and allowed by your account. It does not register new bundle IDs, change their
App Group settings or register devices. Your admin's existing setup remains a
prerequisite. This follows Xcode's automatic export flow rather than pinning
particular manually supplied profile files.

After export, the script verifies signatures and checks that both bundles have
the requested IDs, team and shared group. It preserves release version and
build numbers, refuses to overwrite an existing output, and does not install
the IPA or upload it to App Store Connect. Cloud signing still communicates
with Apple's signing service, as Xcode normally does.

If the latest release has no unsigned iOS IPA yet, the command fails. It does
not silently choose an older release. The checksum detects a damaged download;
it is downloaded from the same release and is not an independent signature.

Test the download, archive preparation, export options and verification without
an Apple account or signing key. Account-backed export is mocked in these tests:

```bash
python3 -m unittest discover -s ios -p 'test_sign_release.py'
```

## Install and configure the farm

Install the signed IPA with Xcode's Devices and Simulators window or Apple
Configurator. For a source build, Xcode installs it directly. Apple's
[registered-device guide](https://developer.apple.com/documentation/xcode/distributing-your-app-to-registered-devices)
describes signing and installation.

Install before the reservation baseline. Keep the same bundle IDs, signing
team and App Group for upgrades. The provider defaults to
`group.com.dinispimpao.yard.drop`. If you changed the group, set it on that device:

```yaml
options:
  drop_app_group: group.com.example.yarddrop
```

Reserve the iPhone, share a file to YARD - Device Farm, and choose Send to YARD
browser. It appears automatically in Receive shared files. Release and reserve
again to verify the old batch is gone. Files saved through the Files picker use
the selected destination; configure `cleanup_app_paths` for Runner Documents if
those copies should also be wiped on release. Renew Personal Team provisioning
before its seven-day expiry.
