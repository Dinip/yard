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
SUBPROCESS_RUN = subprocess.run
class SigningTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def make_ipa(self, path):
        with zipfile.ZipFile(path, "w") as archive:
            for bundle in ["Payload/Runner.app", "Payload/Runner.app/PlugIns/ShareExtension.appex"]:
                archive.writestr(f"{bundle}/Info.plist", plistlib.dumps({
                    "CFBundleIdentifier": "old.id", "YardAppGroup": "group.old",
                    "CFBundleVersion": "123", "CFBundleShortVersionString": "1.0.0",
                }))
                archive.writestr(f"{bundle}/embedded.mobileprovision", b"old profile")
            archive.writestr("Payload/Runner.app/Frameworks/App.framework/App", b"framework")

    def test_refuses_to_overwrite_before_accessing_xcode_or_github(self):
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

    def test_prepares_both_bundles_with_app_group_for_xcode_export(self):
        ipa = self.directory / "unsigned.ipa"
        self.make_ipa(ipa)
        calls = []

        def command(*args):
            calls.append(args)
            if args[0] == "ditto":
                return subprocess.check_output([str(arg) for arg in args])
            return b""

        with patch.object(signing, "run", side_effect=command):
            archive = signing.prepare_archive(ipa, self.directory, IDS, GROUP, TEAM)
        app = archive / "Products/Applications/Runner.app"
        for bundle, bundle_id in zip([app, app / "PlugIns/ShareExtension.appex"], IDS):
            info = plistlib.loads((bundle / "Info.plist").read_bytes())
            self.assertEqual(info["CFBundleIdentifier"], bundle_id)
            self.assertEqual(info["YardAppGroup"], GROUP)
            self.assertEqual(info["CFBundleVersion"], "123")
            self.assertEqual(info["CFBundleShortVersionString"], "1.0.0")
            self.assertFalse((bundle / "embedded.mobileprovision").exists())
        properties = plistlib.loads((archive / "Info.plist").read_bytes())["ApplicationProperties"]
        self.assertEqual(properties["Team"], TEAM)
        self.assertEqual(properties["CFBundleIdentifier"], IDS[0])
        self.assertEqual(properties["ApplicationPath"], "Applications/Runner.app")
        codesign = [call for call in calls if call[0] == "codesign"]
        self.assertEqual([call[-1].name for call in codesign], [
            "App.framework", "ShareExtension.appex", "Runner.app",
        ])
        for call in codesign:
            self.assertEqual(call[call.index("--sign") + 1], "-")
        for call in codesign[1:]:
            entitlements = plistlib.loads(call[-2].read_bytes())
            self.assertEqual(entitlements, {"com.apple.security.application-groups": [GROUP]})

    def exported_ipa(self, command, check, **kwargs):
        if command[0] != "xcodebuild":
            return SUBPROCESS_RUN(command, check=check, **kwargs)
        options = Path(command[command.index("-exportOptionsPlist") + 1])
        values = plistlib.loads(options.read_bytes())
        self.assertEqual(values, {
            "method": "release-testing", "destination": "export",
            "signingStyle": "automatic", "teamID": TEAM,
            "manageAppVersionAndBuildNumber": False,
        })
        self.assertTrue(check)
        self.assertIn("-allowProvisioningUpdates", command)
        exported = Path(command[command.index("-exportPath") + 1])
        exported.mkdir(exist_ok=True)
        ipa = exported / "Runner.ipa"
        with zipfile.ZipFile(ipa, "w") as archive:
            for bundle, bundle_id in zip(
                ["Payload/Runner.app", "Payload/Runner.app/PlugIns/ShareExtension.appex"], IDS
            ):
                archive.writestr(f"{bundle}/Info.plist", plistlib.dumps({
                    "CFBundleIdentifier": bundle_id, "YardAppGroup": GROUP,
                }))

    def verified_command(self, *args):
        if args[0] == "ditto":
            return subprocess.check_output([str(arg) for arg in args])
        if "--display" in args:
            bundle_id = IDS[1] if args[-1].suffix == ".appex" else IDS[0]
            return plistlib.dumps({
                "application-identifier": f"{TEAM}.{bundle_id}",
                "com.apple.developer.team-identifier": TEAM,
                "com.apple.security.application-groups": [GROUP],
            })
        return b""

    def test_exports_with_xcode_account_and_verifies_both_targets(self):
        with patch.object(signing.subprocess, "run", side_effect=self.exported_ipa):
            with patch.object(signing, "run", side_effect=self.verified_command) as command:
                ipa = signing.export_archive(self.directory / "Runner.xcarchive", self.directory, IDS, GROUP, TEAM)
        self.assertTrue(zipfile.is_zipfile(ipa))
        displayed = [call.args[-1].name for call in command.call_args_list if "--display" in call.args]
        self.assertEqual(displayed, ["Runner.app", "ShareExtension.appex"])
        self.assertTrue(any("--verify" in call.args for call in command.call_args_list))

    def test_refuses_export_if_extension_loses_group_or_uses_wrong_team(self):
        for changed in [{"com.apple.security.application-groups": []},
                        {"com.apple.developer.team-identifier": "OTHER12345"},
                        {"application-identifier": f"{TEAM}.wrong.id"}]:
            with self.subTest(changed=changed):
                def command(*args):
                    result = self.verified_command(*args)
                    if "--display" in args and args[-1].suffix == ".appex":
                        result = plistlib.dumps({**plistlib.loads(result), **changed})
                    return result

                with patch.object(signing.subprocess, "run", side_effect=self.exported_ipa):
                    with patch.object(signing, "run", side_effect=command):
                        with self.assertRaisesRegex(ValueError, "unexpected IDs, team or App Group"):
                            signing.export_archive(self.directory / "Runner.xcarchive", self.directory, IDS, GROUP, TEAM)

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
                        signing.prepare_archive(ipa, self.directory, IDS, GROUP, TEAM)
                    command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
