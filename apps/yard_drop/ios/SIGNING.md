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
   Keep the `YARD - Device Farm` display name and `yard_drop` bundle name so
   the browser can discover the signed app.
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

Use a Mac with a signing identity and its private key in Keychain, plus two
profiles from your own team. For an ad hoc deployment, both profiles must list
the destination iPhones. A Personal Team can use profiles Xcode generated for
the corresponding source targets and device, subject to their seven-day expiry.

Verify the release checksum first. Work in a fresh directory and unpack the
IPA:

```bash
YARD_UNSIGNED_IPA='yard-drop-0.6.5+101-ios-unsigned.ipa' # use your downloaded filename
shasum -a 256 -c "$YARD_UNSIGNED_IPA.sha256"
mkdir signing-work
ditto -x -k "$YARD_UNSIGNED_IPA" signing-work
security find-identity -v -p codesigning
```

Copy your profiles into the current directory as `Runner.mobileprovision` and
`ShareExtension.mobileprovision`. Decode them and extract their entitlements:

```bash
security cms -D -i Runner.mobileprovision > Runner-profile.plist
security cms -D -i ShareExtension.mobileprovision > ShareExtension-profile.plist
/usr/libexec/PlistBuddy -x -c 'Print :Entitlements' Runner-profile.plist > Runner-entitlements.plist
/usr/libexec/PlistBuddy -x -c 'Print :Entitlements' ShareExtension-profile.plist > ShareExtension-entitlements.plist
```

The profiles must have explicit App IDs and the same single App Group. This
updates the unsigned bundles to match the profiles, including custom IDs:

```bash
python3 <<'PY'
from pathlib import Path
import plistlib, shutil

app = Path('signing-work/Payload/Runner.app')
extension = app / 'PlugIns/ShareExtension.appex'
profiles = [plistlib.loads(Path(f'{name}-profile.plist').read_bytes())
            for name in ['Runner', 'ShareExtension']]
groups = [p['Entitlements'].get('com.apple.security.application-groups', [])
          for p in profiles]
assert len(groups[0]) == 1 and groups[0] == groups[1], 'Both profiles need the same App Group'
assert profiles[0]['TeamIdentifier'] == profiles[1]['TeamIdentifier'], 'Use the same team'
ids = [p['Entitlements']['application-identifier'].split('.', 1)[1]
       for p in profiles]
assert all('*' not in value for value in ids), 'Use explicit App IDs'
assert ids[1].startswith(ids[0] + '.'), 'The extension ID must be below the app ID'
for name, bundle, bundle_id in zip(['Runner', 'ShareExtension'], [app, extension], ids):
    path = bundle / 'Info.plist'
    info = plistlib.loads(path.read_bytes())
    info['CFBundleIdentifier'] = bundle_id
    info['YardAppGroup'] = groups[0][0]
    path.write_bytes(plistlib.dumps(info))
    shutil.copyfile(f'{name}.mobileprovision', bundle / 'embedded.mobileprovision')
print('Bundles match the supplied profiles.')
PY
```

Sign nested code first, then the extension, then the containing app. Replace
the identity placeholder with the identity name reported by Keychain:

```bash
YARD_SIGNING_IDENTITY='Apple Development: <your identity>'
YARD_APP='signing-work/Payload/Runner.app'
YARD_EXTENSION="$YARD_APP/PlugIns/ShareExtension.appex"
for code in "$YARD_APP"/Frameworks/* "$YARD_EXTENSION"/Frameworks/*; do
  [ -e "$code" ] || continue
  codesign --force --sign "$YARD_SIGNING_IDENTITY" "$code"
done
codesign --force --sign "$YARD_SIGNING_IDENTITY" --generate-entitlement-der \
  --entitlements ShareExtension-entitlements.plist "$YARD_EXTENSION"
codesign --force --sign "$YARD_SIGNING_IDENTITY" --generate-entitlement-der \
  --entitlements Runner-entitlements.plist "$YARD_APP"
codesign --verify --deep --strict --verbose=2 "$YARD_APP"
cd signing-work
zip -qry ../yard-drop-signed.ipa Payload
```

Use an Apple Distribution identity with distribution profiles when signing for
ad hoc deployment. Signing locally does not extend a profile's expiry or add
devices to it. A signing tool must preserve the Share Extension, sign both
targets with their respective profiles, and preserve their shared App Group.

## Install and configure the farm

Install the signed IPA with Xcode's Devices and Simulators window or Apple
Configurator. For a source build, Xcode installs it directly. Apple's
[registered-device guide](https://developer.apple.com/documentation/xcode/distributing-your-app-to-registered-devices)
describes signing and installation.

Install before the reservation baseline. Keep the same bundle IDs, signing
team and App Group for upgrades. In the provider's `cleanup_app_paths`, use the
**actual signed Runner bundle ID**, even when it differs from the release's
default. The browser detects the installed companion's ID by its product name.

Before reserving the device for another user, enable `cleanup.enabled` and
`cleanup.wipeFolders`, test that published Documents files are removed, and
confirm the extension still shares successfully after a restart. Renew
Personal Team provisioning before its seven-day expiry.
