import json
import os
from pathlib import Path
import stat
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import pairing


class CaptureTests(unittest.TestCase):
    def test_ambiguous_phone_instances_require_explicit_selection(self):
        record = dict(instance="PHONE-A", host="phone.local", port=49152, txt=["identifier=PHONE-A", "authTag=test"])
        other = dict(record, instance="PHONE-B")
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "phone.json"
            args = SimpleNamespace(interface="eth0", instance=None, record=target, iphone="10.20.0.5", timeout=2)
            with patch.object(pairing, "Advertisement") as client:
                client.return_value.discover.return_value = [(record, args.iphone), (other, args.iphone)]
                with self.assertRaisesRegex(ValueError, "Multiple instances"):
                    pairing.capture_device(args)
                self.assertFalse(target.exists())
                args.instance = "PHONE-B"
                pairing.capture_device(args)
            self.assertEqual(json.loads(target.read_text())["record"], other)

    def test_linux_capture_selects_only_the_requested_phone_ip(self):
        record = dict(instance="PHONE-UUID", host="phone.local", port=49152, txt=["identifier=PHONE-UUID", "authTag=test"])
        other = dict(record, instance="OTHER-PHONE")
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "phone.json"
            args = SimpleNamespace(interface="eth0", instance=None, record=target, iphone="10.10.50.100", timeout=2)
            with patch.object(pairing, "Advertisement") as client:
                client.return_value.discover.return_value = [(other, "10.10.50.200"), (record, "10.10.50.100")]
                pairing.capture_device(args)
                client.return_value.close.assert_called_once()
            saved = json.loads(target.read_text())
            self.assertEqual(saved, dict(iphone="10.10.50.100", record=record))
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)

    def test_captures_native_host_identity_without_generating_pairing_keys(self):
        browse = "12:00:00.000 Add 2 11 local. _remotepairing-pairable-host._tcp. HOST-UUID\n"
        resolved = "12:00:00.000 HOST-UUID can be reached at host.local.:55732 (interface 11)\n identifier=HOST-UUID authTag=test name=Developer\\ Mac model=Mac15,12 flags=1 ver=26 minVer=17\n"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "host.json"
            args = SimpleNamespace(interface="en0", instance=None, record=target)
            with patch.object(pairing.bridge, "dns_query", side_effect=[browse, resolved]):
                pairing.capture(args)
            record = json.loads(target.read_text())
            self.assertEqual(record["port"], 55732)
            self.assertIn("identifier=HOST-UUID", record["txt"])
            self.assertIn("authTag=test", record["txt"])
            self.assertIn("name=Developer Mac", record["txt"])
            self.assertEqual(set(record), {"instance", "host", "port", "txt"})
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)


@unittest.skipUnless(
    sys.platform == "linux" and os.environ.get("YARD_IOS_TEST_AVAHI") == "1",
    "set YARD_IOS_TEST_AVAHI=1 on Linux with system D-Bus and Avahi",
)
class AvahiTests(unittest.TestCase):
    def test_entry_group_lifetime(self):
        advertisement = pairing.Advertisement()
        try:
            self.assertEqual(advertisement.state(), 0)
        finally:
            advertisement.close()
        self.assertIsNone(advertisement.bus)


if __name__ == "__main__":
    unittest.main()
