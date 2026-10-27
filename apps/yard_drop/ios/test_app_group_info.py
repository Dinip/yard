from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("generate-app-group-info.py")


class AppGroupInfoTests(unittest.TestCase):
    def test_custom_group_replaces_stale_value_without_changing_sources(self):
        with tempfile.TemporaryDirectory(prefix="yard app group ") as directory:
            root = Path(directory)
            source, entitlements, output = [root / name for name in ["Info.plist", "App.entitlements", "Generated.plist"]]
            info = {"YardAppGroup": "group.old", "CFBundleIdentifier": "$(PRODUCT_BUNDLE_IDENTIFIER)"}
            source.write_bytes(plistlib.dumps(info))
            for group in ["group.company.drop", "group.other.drop"]:
                with self.subTest(group=group):
                    entitlements.write_bytes(plistlib.dumps({"com.apple.security.application-groups": [group]}))
                    subprocess.run([sys.executable, str(SCRIPT), str(source), str(entitlements), str(output)], check=True)
                    self.assertEqual(plistlib.loads(output.read_bytes()), {**info, "YardAppGroup": group})
                    self.assertEqual(plistlib.loads(source.read_bytes()), info)
                    self.assertEqual(plistlib.loads(entitlements.read_bytes()), {"com.apple.security.application-groups": [group]})

    def test_missing_ambiguous_or_invalid_groups_fail_without_generating_info(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, entitlements, output = [root / name for name in ["Info.plist", "App.entitlements", "Generated.plist"]]
            source.write_bytes(plistlib.dumps({}))
            for groups in [[], ["group.one", "group.two"], "group.string", [42], ["wrong.prefix"]]:
                with self.subTest(groups=groups):
                    entitlements.write_bytes(plistlib.dumps({"com.apple.security.application-groups": groups}))
                    result = subprocess.run([sys.executable, str(SCRIPT), str(source), str(entitlements), str(output)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Select exactly one App Group", result.stderr)
                    self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
