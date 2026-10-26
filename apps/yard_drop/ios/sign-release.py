#!/usr/bin/env python3
"""Download the latest YARD Drop IPA and sign it with locally provisioned IDs."""

import argparse
from datetime import datetime, timezone
import hashlib
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile


def run(*args):
    return subprocess.check_output([str(arg) for arg in args])


def select_profiles(team, bundle_ids, group, directories):
    identities = set(re.findall(
        r"\b[0-9A-F]{40}\b",
        run("security", "find-identity", "-v", "-p", "codesigning").decode().upper(),
    ))
    matches = [[], []]
    for directory in directories:
        for path in sorted(directory.glob("*.mobileprovision")):
            profile = plistlib.loads(run("security", "cms", "-D", "-i", path))
            entitlements = profile.get("Entitlements", {})
            if (
                profile.get("TeamIdentifier") != [team]
                or entitlements.get("com.apple.developer.team-identifier") != team
                or profile["ExpirationDate"] <= datetime.now(timezone.utc).replace(tzinfo=None)
                or group not in entitlements.get("com.apple.security.application-groups", [])
                or not (profile.get("ProvisionedDevices") or profile.get("ProvisionsAllDevices"))
            ):
                continue
            certificates = {
                hashlib.sha1(cert).hexdigest().upper()
                for cert in profile.get("DeveloperCertificates", [])
            } & identities
            for index, bundle_id in enumerate(bundle_ids):
                app_id = entitlements.get("application-identifier", "")
                if app_id.partition(".")[2] == bundle_id and certificates:
                    matches[index].append((profile, path, certificates))
    for candidates in matches:
        candidates.sort(key=lambda item: item[0]["ExpirationDate"], reverse=True)
    for app in matches[0]:
        for extension in matches[1]:
            common = app[2] & extension[2]
            app_devices = set(app[0].get("ProvisionedDevices", []))
            extension_devices = set(extension[0].get("ProvisionedDevices", []))
            prefixes = [item[0]["Entitlements"]["application-identifier"].partition(".")[0]
                        for item in [app, extension]]
            if common and prefixes[0] == prefixes[1] and (
                app[0].get("ProvisionsAllDevices")
                or extension[0].get("ProvisionsAllDevices")
                or app_devices & extension_devices
            ):
                return [app[:2], extension[:2]], sorted(common)[0]
    raise ValueError(
        "No compatible profiles and Keychain private key found. In Xcode, provision "
        f"{bundle_ids[0]} and {bundle_ids[1]} for team {team}, both with App Group "
        f"{group} and a common destination device. See ios/SIGNING.md."
    )


def download_release(directory):
    tag = run("gh", "release", "view", "--repo", "Dinip/yard",
              "--json", "tagName", "--jq", ".tagName").decode().strip()
    print(f"Downloading {tag}", flush=True)
    run("gh", "release", "download", tag, "--repo", "Dinip/yard",
        "--pattern", "yard-drop-*-ios-unsigned.ipa",
        "--pattern", "yard-drop-*-ios-unsigned.ipa.sha256", "--dir", directory)
    archives = list(directory.glob("*-ios-unsigned.ipa"))
    if len(archives) != 1:
        raise ValueError(f"Expected one unsigned iOS IPA in release {tag}.")
    ipa = archives[0]
    checksum = ipa.with_suffix(".ipa.sha256").read_text()
    match = re.fullmatch(r"([0-9a-fA-F]{64}) [ *]" + re.escape(ipa.name) + r"\n?", checksum)
    digest = hashlib.sha256()
    with ipa.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    if not match or digest.hexdigest() != match[1].lower():
        raise ValueError("The downloaded IPA does not match its release checksum.")
    return ipa


def sign_release(ipa, directory, bundle_ids, group, profiles, identity):
    # Validate paths before ditto extracts an archive into the signing workspace.
    with zipfile.ZipFile(ipa) as archive:
        for entry in archive.infolist():
            path = PurePosixPath(entry.filename)
            if (path.is_absolute() or ".." in path.parts
                    or stat.S_ISLNK(entry.external_attr >> 16)):
                raise ValueError(f"Unsafe IPA entry: {entry.filename}")
    unpacked = directory / "unpacked"
    run("ditto", "-x", "-k", ipa, unpacked)
    app = unpacked / "Payload/Runner.app"
    extension = app / "PlugIns/ShareExtension.appex"
    if (list((unpacked / "Payload").glob("*.app")) != [app]
            or list((app / "PlugIns").iterdir()) != [extension]):
        raise ValueError("Expected Runner.app with exactly one ShareExtension.appex.")
    entitlement_paths = []
    for bundle, bundle_id, (profile, profile_path) in zip(
        [app, extension], bundle_ids, profiles
    ):
        info_path = bundle / "Info.plist"
        info = plistlib.loads(info_path.read_bytes())
        info["CFBundleIdentifier"] = bundle_id
        info["YardAppGroup"] = group
        info_path.write_bytes(plistlib.dumps(info))
        shutil.copyfile(profile_path, bundle / "embedded.mobileprovision")
        allowed = profile["Entitlements"]
        entitlements = {
            "application-identifier": allowed["application-identifier"],
            "com.apple.developer.team-identifier": allowed["com.apple.developer.team-identifier"],
            "com.apple.security.application-groups": [group],
            "get-task-allow": allowed.get("get-task-allow", False),
        }
        entitlement_path = directory / f"{bundle.name}.entitlements"
        entitlement_path.write_bytes(plistlib.dumps(entitlements))
        entitlement_paths.append(entitlement_path)
        print(f"{bundle_id}: {profile['Name']}, expires {profile['ExpirationDate']}", flush=True)
    nested = [path for path in app.rglob("*") if path.suffix in {".framework", ".dylib"}]
    for code in sorted(nested, key=lambda path: len(path.parts), reverse=True):
        run("codesign", "--force", "--sign", identity, "--timestamp=none", code)
    for bundle, entitlements in reversed(list(zip([app, extension], entitlement_paths))):
        run("codesign", "--force", "--sign", identity, "--timestamp=none",
            "--generate-entitlement-der", "--entitlements", entitlements, bundle)
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
    signed = directory / "signed.ipa"
    run("ditto", "-c", "-k", "--keepParent", unpacked / "Payload", signed)
    return signed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--team", required=True, help="Apple Team ID, not the account email")
    parser.add_argument("--bundle-id", required=True, help="Runner bundle ID")
    parser.add_argument("--extension-bundle-id", required=True, help="Share Extension bundle ID")
    parser.add_argument("--app-group", required=True, help="App Group shared by both targets")
    parser.add_argument("--output", type=Path, default=Path("yard-drop-signed.ipa"))
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Z0-9]{10}", args.team):
        parser.error("--team must be a 10-character Apple Team ID")
    for value in [args.bundle_id, args.extension_bundle_id, args.app_group]:
        if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", value):
            parser.error(f"Invalid identifier: {value}")
    if not args.extension_bundle_id.startswith(args.bundle_id + "."):
        parser.error("The extension bundle ID must start with the Runner bundle ID and a dot")
    if not args.app_group.startswith("group."):
        parser.error("--app-group must start with group.")
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        parser.error(f"Output already exists: {output}")
    if sys.platform != "darwin":
        parser.error("Signing requires macOS with Xcode command-line tools")
    for command in ["gh", "security", "codesign", "ditto"]:
        if not shutil.which(command):
            parser.error(f"Required command not found: {command}")
    home = Path.home()
    bundle_ids = [args.bundle_id, args.extension_bundle_id]
    profiles, identity = select_profiles(args.team, bundle_ids, args.app_group, [
        home / "Library/Developer/Xcode/UserData/Provisioning Profiles",
        home / "Library/MobileDevice/Provisioning Profiles",
    ])
    with tempfile.TemporaryDirectory(prefix=".yard-drop-", dir=output.parent) as temporary:
        directory = Path(temporary)
        ipa = download_release(directory)
        signed = sign_release(ipa, directory, bundle_ids, args.app_group, profiles, identity)
        os.link(signed, output)
    print(f"Signed IPA: {output}\nProvider device option: drop_app_group: {args.app_group}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError,
            plistlib.InvalidFileException, zipfile.BadZipFile) as error:
        sys.exit(f"Signing failed: {error}")
