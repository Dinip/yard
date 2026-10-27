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

    def test_refuses_to_overwrite_before_accessing_xcode(self):
        ipa = self.directory / "unsigned.ipa"
        self.make_ipa(ipa)
        output = self.directory / "signed.ipa"
        output.write_bytes(b"keep this IPA")
        arguments = ["sign-release.py", str(ipa), "--team", TEAM, "--bundle-id", IDS[0],
                     "--extension-bundle-id", IDS[1], "--app-group", GROUP,
                     "--output", str(output)]
        with patch.object(sys, "argv", arguments), patch.object(sys, "stderr", io.StringIO()):
            with patch.object(signing, "run") as command:
                with self.assertRaises(SystemExit) as error:
                    signing.main()
                self.assertEqual(error.exception.code, 2)
                command.assert_not_called()
        self.assertEqual(output.read_bytes(), b"keep this IPA")

    def test_signs_supplied_local_file_without_modifying_it(self):
        ipa = self.directory / "my unsigned app.ipa"
        self.make_ipa(ipa)
        original = ipa.read_bytes()
        output = self.directory / "signed.ipa"
        options = self.directory / "working-export.plist"
        options.write_bytes(plistlib.dumps({"method": "debugging", "signingStyle": "manual"}))
        arguments = ["sign-release.py", str(ipa), "--team", TEAM, "--bundle-id", IDS[0],
                     "--extension-bundle-id", IDS[1], "--app-group", GROUP,
                     "--output", str(output), "--export-options", str(options)]

        def export(archive, directory, bundle_ids, group, team, export_options, export_method):
            self.assertEqual(export_options, options)
            signed = directory / "signed.ipa"
            signed.write_bytes(b"signed fixture")
            return signed

        with patch.object(sys, "argv", arguments), patch.object(sys, "platform", "darwin"):
            with patch.object(signing.shutil, "which", return_value="tool") as which:
                with patch.object(signing, "prepare_archive", return_value=Path("Runner.xcarchive")) as prepare:
                    with patch.object(signing, "export_archive", side_effect=export):
                        signing.main()
        self.assertEqual(prepare.call_args.args[0], ipa)
        self.assertEqual([call.args[0] for call in which.call_args_list], ["xcodebuild", "codesign", "ditto"])
        self.assertEqual(output.read_bytes(), b"signed fixture")
        self.assertEqual(ipa.read_bytes(), original)

    def test_refuses_missing_input_or_directory_before_accessing_xcode(self):
        for ipa in [self.directory / "missing.ipa", self.directory]:
            with self.subTest(ipa=ipa):
                arguments = ["sign-release.py", str(ipa), "--team", TEAM, "--bundle-id", IDS[0],
                             "--extension-bundle-id", IDS[1], "--app-group", GROUP]
                with patch.object(sys, "argv", arguments), patch.object(sys, "stderr", io.StringIO()):
                    with patch.object(signing, "prepare_archive") as prepare:
                        with self.assertRaises(SystemExit) as error:
                            signing.main()
                        self.assertEqual(error.exception.code, 2)
                        prepare.assert_not_called()

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
            "method": "debugging", "destination": "export",
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

    def test_reuses_successful_xcode_settings_without_modifying_source_or_uploading(self):
        source = self.directory / "working-export.plist"
        values = {
            "method": "debugging", "signingStyle": "manual", "teamID": TEAM,
            "signingCertificate": "Apple Development",
            "provisioningProfiles": {IDS[0]: "Runner profile", IDS[1]: "Extension profile"},
            "destination": "upload", "manageAppVersionAndBuildNumber": True,
        }
        source.write_bytes(plistlib.dumps(values))

        def export(command, check):
            options = Path(command[command.index("-exportOptionsPlist") + 1])
            actual = plistlib.loads(options.read_bytes())
            self.assertEqual(actual, {
                **values, "destination": "export", "manageAppVersionAndBuildNumber": False,
            })
            raise RuntimeError("Stop before account-backed export")

        with patch.object(signing.subprocess, "run", side_effect=export):
            with self.assertRaisesRegex(RuntimeError, "Stop before account-backed export"):
                signing.export_archive(Path("Runner.xcarchive"), self.directory, IDS, GROUP, TEAM, source)
        self.assertEqual(plistlib.loads(source.read_bytes()), values)

    def test_maps_flutter_export_methods_without_uploading(self):
        for method, expected in [
            ("development", "debugging"), ("ad-hoc", "release-testing"),
            ("enterprise", "enterprise"), ("app-store", "app-store-connect"),
        ]:
            with self.subTest(method=method):
                def export(command, check):
                    options = Path(command[command.index("-exportOptionsPlist") + 1])
                    actual = plistlib.loads(options.read_bytes())
                    self.assertEqual(actual["method"], expected)
                    self.assertEqual(actual["destination"], "export")
                    raise RuntimeError("Stop before account-backed export")

                with patch.object(signing.subprocess, "run", side_effect=export):
                    with self.assertRaisesRegex(RuntimeError, "Stop before account-backed export"):
                        signing.export_archive(Path("Runner.xcarchive"), self.directory, IDS,
                                               GROUP, TEAM, export_method=method)

    def test_export_method_and_options_cannot_be_combined(self):
        arguments = ["sign-release.py", "unsigned.ipa", "--team", TEAM,
                     "--bundle-id", IDS[0], "--extension-bundle-id", IDS[1],
                     "--app-group", GROUP, "--export-method", "development",
                     "--export-options", "working.plist"]
        with patch.object(sys, "argv", arguments), patch.object(sys, "stderr", io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                signing.main()
        self.assertEqual(error.exception.code, 2)

    def test_refuses_invalid_or_mismatched_export_options_before_contacting_apple(self):
        source = self.directory / "invalid-export.plist"
        for values in [[], {"teamID": "OTHER12345"}]:
            with self.subTest(values=values):
                source.write_bytes(plistlib.dumps(values))
                with patch.object(signing.subprocess, "run") as export:
                    with self.assertRaises(ValueError) as error:
                        signing.export_archive(Path("Runner.xcarchive"), self.directory, IDS, GROUP, TEAM, source)
                    self.assertNotIn(TEAM, str(error.exception))
                    self.assertNotIn("OTHER12345", str(error.exception))
                    export.assert_not_called()

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
