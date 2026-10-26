#!/usr/bin/env python3
"""Sign a local unsigned YARD Drop IPA using your Xcode account."""

import argparse
from datetime import datetime, timezone
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


def prepare_archive(ipa, directory, bundle_ids, group, team):
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
    for bundle, bundle_id in zip([app, extension], bundle_ids):
        info_path = bundle / "Info.plist"
        info = plistlib.loads(info_path.read_bytes())
        info["CFBundleIdentifier"] = bundle_id
        info["YardAppGroup"] = group
        info_path.write_bytes(plistlib.dumps(info))
        (bundle / "embedded.mobileprovision").unlink(missing_ok=True)
    entitlements = directory / "app-group.entitlements"
    entitlements.write_bytes(plistlib.dumps({"com.apple.security.application-groups": [group]}))
    nested = [path for path in app.rglob("*") if path.suffix in {".framework", ".dylib"}]
    for code in sorted(nested, key=lambda path: len(path.parts), reverse=True):
        run("codesign", "--force", "--sign", "-", "--timestamp=none", code)
    # Export reads capabilities from signatures, so seed both bundles without an Apple certificate.
    for bundle in [extension, app]:
        run("codesign", "--force", "--sign", "-", "--timestamp=none",
            "--generate-entitlement-der", "--entitlements", entitlements, bundle)
    archive = directory / "Runner.xcarchive"
    applications = archive / "Products/Applications"
    applications.mkdir(parents=True)
    shutil.move(app, applications / app.name)
    info = plistlib.loads((applications / app.name / "Info.plist").read_bytes())
    (archive / "Info.plist").write_bytes(plistlib.dumps({
        "ArchiveVersion": 2,
        "CreationDate": datetime.now(timezone.utc).replace(tzinfo=None),
        "Name": "Runner",
        "SchemeName": "Runner",
        "ApplicationProperties": {
            "ApplicationPath": "Applications/Runner.app",
            "CFBundleIdentifier": bundle_ids[0],
            "CFBundleShortVersionString": info["CFBundleShortVersionString"],
            "CFBundleVersion": info["CFBundleVersion"],
            "SigningIdentity": "-",
            "Team": team,
        },
    }))
    return archive


def export_archive(archive, directory, bundle_ids, group, team, export_options=None,
                   export_method="development"):
    values = plistlib.loads(export_options.read_bytes()) if export_options else {
        "method": {
            "development": "debugging", "ad-hoc": "release-testing",
            "enterprise": "enterprise", "app-store": "app-store-connect",
        }[export_method],
        "signingStyle": "automatic",
    }
    if not isinstance(values, dict):
        raise ValueError("Export options must be a plist dictionary.")
    if values.get("teamID", team) != team:
        raise ValueError("Export options use a different team than --team.")
    values.update({
        "teamID": team,
        "destination": "export",
        "manageAppVersionAndBuildNumber": False,
    })
    options = directory / "ExportOptions.plist"
    options.write_bytes(plistlib.dumps(values))
    exported = directory / "exported"
    subprocess.run([
        "xcodebuild", "-exportArchive", "-archivePath", str(archive),
        "-exportOptionsPlist", str(options), "-exportPath", str(exported),
        "-allowProvisioningUpdates",
    ], check=True)
    ipas = list(exported.glob("*.ipa"))
    if len(ipas) != 1:
        raise ValueError("Xcode did not export exactly one signed IPA.")
    verified = directory / "verified"
    run("ditto", "-x", "-k", ipas[0], verified)
    app = verified / "Payload/Runner.app"
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", app)
    for bundle, bundle_id in zip([app, app / "PlugIns/ShareExtension.appex"], bundle_ids):
        entitlements = plistlib.loads(run("codesign", "--display", "--entitlements", "-", "--xml", bundle))
        info = plistlib.loads((bundle / "Info.plist").read_bytes())
        if (
            info["CFBundleIdentifier"] != bundle_id
            or info.get("YardAppGroup") != group
            or entitlements.get("com.apple.developer.team-identifier") != team
            or entitlements.get("application-identifier", "").partition(".")[2] != bundle_id
            or entitlements.get("com.apple.security.application-groups") != [group]
        ):
            raise ValueError(f"Xcode exported unexpected IDs, team or App Group for {bundle_id}.")
    return ipas[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ipa", type=Path, help="Path to the unsigned YARD Drop IPA")
    parser.add_argument("--team", required=True, help="Apple Team ID, not the account email")
    parser.add_argument("--bundle-id", required=True, help="Runner bundle ID")
    parser.add_argument("--extension-bundle-id", required=True, help="Share Extension bundle ID")
    parser.add_argument("--app-group", required=True, help="App Group shared by both targets")
    export = parser.add_mutually_exclusive_group()
    export.add_argument("--export-method", choices=["development", "ad-hoc", "enterprise", "app-store"],
                        default="development", help="Signing method, defaults to development")
    export.add_argument("--export-options", type=Path, help="ExportOptions.plist from a successful Xcode export")
    parser.add_argument("--output", type=Path, default=Path("yard-drop-signed.ipa"))
    args = parser.parse_args()
    ipa = args.ipa.absolute()
    if not ipa.is_file():
        parser.error(f"Unsigned IPA is not a file: {ipa}")
    if args.export_options and not args.export_options.is_file():
        parser.error("Export options is not a file")
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
    for command in ["xcodebuild", "codesign", "ditto"]:
        if not shutil.which(command):
            parser.error(f"Required command not found: {command}")
    bundle_ids = [args.bundle_id, args.extension_bundle_id]
    with tempfile.TemporaryDirectory(prefix=".yard-drop-", dir=output.parent) as temporary:
        directory = Path(temporary)
        archive = prepare_archive(ipa, directory, bundle_ids, args.app_group, args.team)
        signed = export_archive(archive, directory, bundle_ids, args.app_group, args.team,
                                args.export_options, args.export_method)
        os.link(signed, output)
    print(f"Signed IPA: {output}\nProvider device option: drop_app_group: {args.app_group}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError,
            plistlib.InvalidFileException, zipfile.BadZipFile) as error:
        sys.exit(f"Signing failed: {error}")
