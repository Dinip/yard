import ipaddress
from pathlib import Path
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import hotspot


CONFIG = dict(
    uuid="c36356dd-8144-4848-a86f-bbe9fc071626", wifi="wlan0", uplink="eth0",
    address="10.77.0.1/24", blocked=list(hotspot.BLOCKED) + ["198.18.0.0/16"],
)


class HotspotTests(unittest.TestCase):
    def test_private_profile_never_stores_plain_passphrase_or_autoconnects(self):
        content = hotspot.profile(CONFIG, "YARD Test", "a-test-password")
        self.assertNotIn("a-test-password", content)
        for setting in ("autoconnect=false", "hidden=true", "ap-isolation=1", "proto=rsn;", "method=link-local", r"ssid=YARD\sTest"):
            self.assertIn(setting, content)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "test.nmconnection"
            hotspot.private_file(path, content)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                hotspot.private_file(path, "replacement")
            self.assertEqual(path.read_text(), content)

    def test_private_and_connected_destinations_are_denied_before_internet_accept(self):
        chains = hotspot.firewall(CONFIG)
        _, _, rules, hooks = next(chain for chain in chains if chain[1] == "YARD_WIFI_FWD")
        permit = rules.index(["-i", "wlan0", "-s", "10.77.0.0/24", "-o", "eth0", "-j", "ACCEPT"])
        for target in ("10.20.0.5", "172.20.1.1", "192.168.1.1", "100.64.1.1", "198.18.5.1"):
            self.assertTrue(any(
                "-d" in rule and rule[-1] == "DROP" and ipaddress.ip_address(target) in ipaddress.ip_network(rule[rule.index("-d") + 1])
                for rule in rules[:permit]
            ), target)
        self.assertEqual(hooks, [("FORWARD", ["-i", "wlan0"]), ("FORWARD", ["-o", "wlan0"])])
        self.assertEqual(next(chain[2] for chain in chains if chain[1] == "YARD_WIFI6_FWD"), [["-j", "DROP"]])

    def test_overlap_refuses_before_any_firewall_change(self):
        args = SimpleNamespace(wifi="wlan0", uplink="eth0", ssid="YARD-Test", address="10.77.0.1/24", block_cidr=[])
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(hotspot, "STATE", Path(directory) / "state"), patch.object(hotspot, "PROFILE", Path(directory) / "profile"):
                with patch.object(hotspot, "output", side_effect=[
                    "existing-wired-profile", "wifi", "yes", "30 (disconnected)",
                    '[{"flags": ["UP"]}]', '[{"dst": "10.77.0.0/24", "dev": "docker0", "scope": "link"}]',
                ]), patch.object(hotspot, "run") as run:
                    with self.assertRaisesRegex(ValueError, "overlaps"):
                        hotspot.prepare(args)
                    run.assert_not_called()

    def test_active_wifi_is_not_replaced(self):
        args = SimpleNamespace(wifi="wlan0", uplink="eth0", ssid="YARD-Test", address="10.77.0.1/24", block_cidr=[])
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(hotspot, "STATE", Path(directory) / "state"), patch.object(hotspot, "PROFILE", Path(directory) / "profile"):
                with patch.object(hotspot, "output", side_effect=["wired", "wifi", "yes", "100 (connected)"]), patch.object(hotspot, "run") as run:
                    with self.assertRaisesRegex(ValueError, "disconnected"):
                        hotspot.prepare(args)
                    run.assert_not_called()

    def test_failed_activation_stops_ap_before_removing_guards(self):
        events = []

        def command(*args, **kwargs):
            events.append(args)
            if "up" in args:
                raise subprocess.CalledProcessError(4, args, stderr="simulated activation failure")
            return SimpleNamespace(returncode=0, stdout="")

        with tempfile.TemporaryDirectory() as directory:
            state, profile = Path(directory) / "state", Path(directory) / "profile"
            with patch.object(hotspot, "STATE", state), patch.object(hotspot, "PROFILE", profile):
                with patch.object(hotspot, "prepare", return_value=CONFIG), patch.object(hotspot.getpass, "getpass", return_value="a-test-password"):
                    with patch.object(hotspot, "install_firewall", side_effect=lambda _: events.append("guards")), patch.object(hotspot, "remove_firewall", side_effect=lambda _: events.append("unguard")):
                        with patch.object(hotspot, "run", side_effect=command), patch.object(hotspot, "output", side_effect=[CONFIG["uuid"], "", CONFIG["uuid"]]):
                            with self.assertRaises(subprocess.CalledProcessError):
                                hotspot.start(SimpleNamespace(ssid="YARD-Test"))
            self.assertLess(events.index("guards"), next(i for i, event in enumerate(events) if isinstance(event, tuple) and "up" in event))
            self.assertLess(next(i for i, event in enumerate(events) if isinstance(event, tuple) and "down" in event), events.index("unguard"))
            self.assertFalse(state.exists())
            self.assertFalse(profile.exists())
            self.assertNotIn("a-test-password", repr(events))

    def test_failed_deactivation_retains_guards_and_recovery_files(self):
        with tempfile.TemporaryDirectory() as directory:
            state, profile = Path(directory) / "state", Path(directory) / "profile"
            state.mkdir()
            (state / "state.json").write_text("recovery")
            profile.write_text("profile")
            with patch.object(hotspot, "STATE", state), patch.object(hotspot, "PROFILE", profile):
                with patch.object(hotspot, "output", return_value=CONFIG["uuid"]), patch.object(hotspot, "run", side_effect=RuntimeError("cannot deactivate")), patch.object(hotspot, "remove_firewall") as remove:
                    with self.assertRaisesRegex(RuntimeError, "cannot deactivate"):
                        hotspot.stop(CONFIG)
                    remove.assert_not_called()
            self.assertTrue(profile.exists())
            self.assertTrue((state / "state.json").exists())


if __name__ == "__main__":
    unittest.main()
