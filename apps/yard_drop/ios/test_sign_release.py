from datetime import datetime, timedelta, timezone
import hashlib
import importlib.util
import io
from pathlib import Path
import plistlib
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("sign_release", Path(__file__).with_name("sign-release.py"))
signing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signing)

TEAM = "ABCDE12345"
IDS = ["com.example.drop", "com.example.drop.ShareExtension"]
GROUP = "group.com.example.drop"
CERTIFICATE = b"test certificate"
IDENTITY = hashlib.sha1(CERTIFICATE).hexdigest().upper()


def profile(bundle_id):
    return {
        "Name": bundle_id,
        "ExpirationDate": datetime.now(timezone.utc).replace(tzinfo=None, microsecond=0) + timedelta(days=7),
        "TeamIdentifier": [TEAM],
        "DeveloperCertificates": [CERTIFICATE],
        "ProvisionedDevices": ["test-iphone"],
        "Entitlements": {
            "application-identifier": f"{TEAM}.{bundle_id}",
            "com.apple.developer.team-identifier": TEAM,
            "com.apple.security.application-groups": [GROUP],
            "get-task-allow": True,
            "keychain-access-groups": [f"{TEAM}.*"],
        },
    }


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.profiles = []
        for index, bundle_id in enumerate(IDS):
            path = self.directory / f"{index}.mobileprovision"
            value = profile(bundle_id)
            path.write_bytes(plistlib.dumps(value))
            self.profiles.append((value, path))

    def security(self, *args):
        if args[1] == "find-identity":
            return f'1) {IDENTITY} "Apple Development: Test"'.encode()
        return Path(args[-1]).read_bytes()

    def test_selects_matching_profiles_and_keychain_identity(self):
        with patch.object(signing, "run", side_effect=self.security):
            profiles, identity = signing.select_profiles(TEAM, IDS, GROUP, [self.directory])
        self.assertEqual(profiles, self.profiles)
        self.assertEqual(identity, IDENTITY)

    def test_refuses_incompatible_profiles(self):
        original = self.profiles[1][0]
        changes = [
            {"TeamIdentifier": ["OTHER12345"]},
            {"ExpirationDate": datetime(2000, 1, 1)},
            {"DeveloperCertificates": [b"another certificate"]},
            {"ProvisionedDevices": ["another-iphone"]},
            {"ProvisionedDevices": []},
            {"Entitlements": {**original["Entitlements"], "application-identifier": f"{TEAM}.*"}},
            {"Entitlements": {**original["Entitlements"], "application-identifier": f"OLDPREFIX1.{IDS[1]}"}},
            {"Entitlements": {**original["Entitlements"], "com.apple.developer.team-identifier": "OTHER12345"}},
            {"Entitlements": {**original["Entitlements"], "com.apple.security.application-groups": []}},
        ]
        for change in changes:
            with self.subTest(change=change):
                value = {**original, **change}
                self.profiles[1][1].write_bytes(plistlib.dumps(value))
                with patch.object(signing, "run", side_effect=self.security):
                    with self.assertRaisesRegex(ValueError, "No compatible profiles"):
                        signing.select_profiles(TEAM, IDS, GROUP, [self.directory])

    def test_requires_private_key_in_keychain(self):
        with patch.object(signing, "run", side_effect=lambda *args: (
            b"0 valid identities found" if args[1] == "find-identity" else Path(args[-1]).read_bytes()
        )):
            with self.assertRaises(ValueError):
                signing.select_profiles(TEAM, IDS, GROUP, [self.directory])

    def test_refuses_to_overwrite_before_accessing_keychain_or_github(self):
        output = self.directory / "signed.ipa"
        output.write_bytes(b"keep this IPA")
        arguments = ["sign-release.py", "--team", TEAM, "--bundle-id", IDS[0],
                     "--extension-bundle-id", IDS[1], "--app-group", GROUP,
                     "--output", str(output)]
        with patch.object(sys, "argv", arguments), patch.object(sys, "stderr", io.StringIO()):
            with patch.object(signing, "run") as command:
                with self.assertRaises(SystemExit) as error:
                    signing.main()
                self.assertEqual(error.exception.code, 2)
                command.assert_not_called()
        self.assertEqual(output.read_bytes(), b"keep this IPA")

    def test_pins_latest_release_and_checks_checksum(self):
        calls = []

        def github(*args):
            calls.append(args)
            if args[2] == "view":
                return b"v1.0.0\n"
            ipa = self.directory / "yard-drop-1.0.0+1-ios-unsigned.ipa"
            ipa.write_bytes(b"unsigned IPA fixture")
            checksum = hashlib.sha256(ipa.read_bytes()).hexdigest()
            ipa.with_suffix(".ipa.sha256").write_text(f"{checksum}  {ipa.name}\n")
            return b""

        with patch.object(signing, "run", side_effect=github):
            ipa = signing.download_release(self.directory)
        self.assertEqual(calls[1][3], "v1.0.0")
        ipa.write_bytes(b"corrupted")
        with patch.object(signing, "run", return_value=b"v1.0.0\n"):
            with self.assertRaisesRegex(ValueError, "checksum"):
                signing.download_release(self.directory)

    def test_updates_both_bundles_and_signs_inside_out(self):
        ipa = self.directory / "unsigned.ipa"
        with zipfile.ZipFile(ipa, "w") as archive:
            for bundle in ["Payload/Runner.app", "Payload/Runner.app/PlugIns/ShareExtension.appex"]:
                archive.writestr(f"{bundle}/Info.plist", plistlib.dumps({
                    "CFBundleIdentifier": "old.id", "YardAppGroup": "group.old",
                    "CFBundleVersion": "123", "CFBundleShortVersionString": "1.0.0",
                }))
            archive.writestr("Payload/Runner.app/Frameworks/App.framework/App", b"framework")
        calls = []

        def command(*args):
            calls.append(args)
            if args[0] == "ditto":
                return subprocess.check_output([str(arg) for arg in args])
            return b""

        with patch.object(signing, "run", side_effect=command):
            signed = signing.sign_release(ipa, self.directory, IDS, GROUP, self.profiles, IDENTITY)
        self.assertTrue(zipfile.is_zipfile(signed))
        with zipfile.ZipFile(signed) as archive:
            for bundle, bundle_id, source in zip(
                ["Payload/Runner.app", "Payload/Runner.app/PlugIns/ShareExtension.appex"],
                IDS, self.profiles,
            ):
                info = plistlib.loads(archive.read(f"{bundle}/Info.plist"))
                self.assertEqual(info["CFBundleIdentifier"], bundle_id)
                self.assertEqual(info["YardAppGroup"], GROUP)
                self.assertEqual(info["CFBundleVersion"], "123")
                self.assertEqual(info["CFBundleShortVersionString"], "1.0.0")
                self.assertEqual(archive.read(f"{bundle}/embedded.mobileprovision"), source[1].read_bytes())
        codesign = [call for call in calls if call[0] == "codesign"]
        self.assertEqual([call[-1].name for call in codesign], [
            "App.framework", "ShareExtension.appex", "Runner.app", "Runner.app",
        ])
        for call in codesign[1:3]:
            entitlements = plistlib.loads(call[-2].read_bytes())
            self.assertEqual(entitlements["com.apple.security.application-groups"], [GROUP])
            self.assertNotIn("keychain-access-groups", entitlements)
            self.assertNotIn("*", entitlements["application-identifier"])
        self.assertIn("--verify", codesign[-1])

    def test_rejects_unsafe_archive_before_extraction(self):
        for name, mode in [("../escape", 0), ("/absolute", 0), ("Payload/link", stat.S_IFLNK)]:
            with self.subTest(name=name):
                ipa = self.directory / "unsafe.ipa"
                with zipfile.ZipFile(ipa, "w") as archive:
                    entry = zipfile.ZipInfo(name)
                    entry.external_attr = mode << 16
                    archive.writestr(entry, b"target")
                with patch.object(signing, "run") as command:
                    with self.assertRaisesRegex(ValueError, "Unsafe IPA entry"):
                        signing.sign_release(ipa, self.directory, IDS, GROUP, self.profiles, IDENTITY)
                    command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
