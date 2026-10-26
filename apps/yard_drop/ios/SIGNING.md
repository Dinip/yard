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

Use a Mac with Python 3.9 or newer, Xcode's command-line tools and the GitHub
CLI. Sign in with `gh auth login` if the repository requires authentication.
Add your Apple account in Xcode Settings and let Xcode create or download
profiles for both bundle IDs and the shared App Group, as described above.
The signing certificate and its private key must be in your login Keychain.
No Flutter installation or source build is needed to sign the downloaded IPA.

From `apps/yard_drop`, run:

```bash
python3 ios/sign-release.py \
  --team ABCDE12345 \
  --bundle-id com.example.yarddrop \
  --extension-bundle-id com.example.yarddrop.ShareExtension \
  --app-group group.com.example.yarddrop \
  --output yard-drop-signed.ipa
```

Use your ten-character Apple Team ID, not your account email. The extension ID
must start with the Runner ID followed by a dot. Both IDs must already have
explicit profiles for that team, with the requested App Group enabled. Changing
these arguments does not register IDs, create profiles or change your Apple
account. Use Xcode to provision new IDs first.

The script selects compatible, unexpired profiles from Xcode's current and
legacy profile directories and a matching identity with a Keychain private key.
Both profiles must authorize at least one common destination device, or permit
all devices. App Store profiles are excluded because they cannot install this
IPA directly. For ad hoc deployment, use profiles containing the destination
iPhones and an Apple Distribution identity. Personal Team profiles retain their
seven-day expiry.

The script pins the latest GitHub release, downloads its unsigned IPA and
SHA-256 checksum, verifies the checksum, and works in a temporary directory.
It updates `CFBundleIdentifier` and `YardAppGroup` in both bundles, embeds their
profiles, and signs the Flutter frameworks, extension and Runner in that order.
Entitlements claim the explicit App ID, team, requested group and debugging
permission allowed by each profile. It verifies the signatures before writing
the output and refuses to overwrite an existing file. Release version and build
numbers are preserved. It does not install the IPA or modify the Xcode project.

If the latest release has no unsigned iOS IPA yet, the command fails. It does
not silently choose an older release. The checksum detects a damaged download;
it is downloaded from the same release and is not an independent signature.

Test the script without an Apple account or signing key:

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
