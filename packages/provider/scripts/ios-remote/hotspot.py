#!/usr/bin/env python3
"""Temporary, hidden NetworkManager AP for the remote Xcode lab PoC."""

import argparse
import getpass
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import uuid


STATE = Path("/run/yard-wifi-poc")
PROFILE = Path("/run/NetworkManager/system-connections/yard-wifi-poc.nmconnection")
NAME = "yard-wifi-poc"
PRIVATE = tuple(ipaddress.ip_network(value) for value in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"))
BLOCKED = (
    "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8",
    "169.254.0.0/16", "172.16.0.0/12", "192.168.0.0/16", "224.0.0.0/4", "240.0.0.0/4",
)


def run(*args, check=True):
    return subprocess.run(args, check=check, text=True, capture_output=True, env={**os.environ, "LC_ALL": "C"})


def output(*args):
    return run(*args).stdout.strip()


def rule_exists(*args):
    result = run(*args, check=False)
    if result.returncode not in (0, 1):
        result.check_returncode()
    return result.returncode == 0


def firewall(config):
    wifi, uplink = config["wifi"], config["uplink"]
    address = ipaddress.ip_interface(config["address"])
    network, host = str(address.network), str(address.ip)
    incoming = [
        ["-m", "conntrack", "--ctstate", "INVALID", "-j", "DROP"],
        ["-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT"],
        ["-p", "udp", "--sport", "68", "--dport", "67", "-j", "ACCEPT"],
        ["-s", network, "-d", host, "-p", "udp", "--dport", "53", "-j", "ACCEPT"],
        ["-s", network, "-d", host, "-p", "tcp", "--dport", "53", "-j", "ACCEPT"],
        ["-s", network, "-p", "udp", "--dport", "5353", "-j", "ACCEPT"],
        ["-s", network, "-d", host, "-p", "tcp", "--dport", "55732", "-j", "ACCEPT"],
        ["-j", "DROP"],
    ]
    forwarding = [
        ["-m", "conntrack", "--ctstate", "INVALID", "-j", "DROP"],
        ["-i", wifi, "!", "-s", network, "-j", "DROP"],
        *[["-i", wifi, "-d", cidr, "-j", "DROP"] for cidr in config["blocked"]],
        ["-i", wifi, "-s", network, "-o", uplink, "-j", "ACCEPT"],
        ["-i", uplink, "-o", wifi, "-d", network, "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT"],
        ["-j", "DROP"],
    ]
    incoming6 = [
        ["-m", "conntrack", "--ctstate", "INVALID", "-j", "DROP"],
        ["-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT"],
        ["-p", "ipv6-icmp", "-j", "ACCEPT"],
        ["-s", "fe80::/10", "-p", "udp", "--dport", "5353", "-j", "ACCEPT"],
        ["-s", "fe80::/10", "-p", "tcp", "--dport", "55732", "-j", "ACCEPT"],
        ["-j", "DROP"],
    ]
    return [
        ("iptables", "YARD_WIFI_IN", incoming, [("INPUT", ["-i", wifi])]),
        ("iptables", "YARD_WIFI_FWD", forwarding, [("FORWARD", ["-i", wifi]), ("FORWARD", ["-o", wifi])]),
        ("ip6tables", "YARD_WIFI6_IN", incoming6, [("INPUT", ["-i", wifi])]),
        ("ip6tables", "YARD_WIFI6_FWD", [["-j", "DROP"]], [("FORWARD", ["-i", wifi]), ("FORWARD", ["-o", wifi])]),
    ]


def install_firewall(config):
    for binary, chain, rules, hooks in firewall(config):
        run(binary, "-w", "5", "-N", chain)
        for rule in rules:
            run(binary, "-w", "5", "-A", chain, *rule)
        for parent, match in hooks:
            run(binary, "-w", "5", "-I", parent, "1", *match, "-j", chain)


def remove_firewall(config):
    for binary, chain, _, hooks in reversed(firewall(config)):
        for parent, match in hooks:
            if rule_exists(binary, "-w", "5", "-C", parent, *match, "-j", chain):
                run(binary, "-w", "5", "-D", parent, *match, "-j", chain)
        if rule_exists(binary, "-w", "5", "-S", chain):
            run(binary, "-w", "5", "-F", chain)
            run(binary, "-w", "5", "-X", chain)


def private_file(path, content):
    with open(path, "x", encoding="utf-8", opener=lambda name, flags: os.open(name, flags, 0o600)) as handle:
        handle.write(content)


def profile(config, ssid, password):
    # A derived PSK avoids putting the passphrase in process arguments or escaping it as keyfile syntax.
    psk = hashlib.pbkdf2_hmac("sha1", password.encode("ascii"), ssid.encode("utf-8"), 4096, 32).hex()
    escaped_ssid = ssid.replace("\\", "\\\\").replace(" ", "\\s")
    return f"""[connection]
id={NAME}
uuid={config['uuid']}
type=wifi
interface-name={config['wifi']}
autoconnect=false

[wifi]
mode=ap
ssid={escaped_ssid}
hidden=true
band={config['band']}
ap-isolation=1
security=802-11-wireless-security

[wifi-security]
key-mgmt=wpa-psk
proto=rsn;
psk={psk}

[ipv4]
method=shared
address1={config['address']}

[ipv6]
method=link-local
"""


def prepare(args):
    if STATE.exists() or PROFILE.exists():
        raise ValueError("A previous hotspot setup exists; run 'down' first.")
    for interface in (args.wifi, args.uplink):
        if not re.fullmatch(r"[A-Za-z0-9_.:-]{1,15}", interface):
            raise ValueError(f"Invalid interface name: {interface}")
    if args.wifi == args.uplink:
        raise ValueError("Wi-Fi and Ethernet uplink must be different interfaces.")
    if not 1 <= len(args.ssid.encode("utf-8")) <= 32 or any(ord(c) < 32 or ord(c) == 127 for c in args.ssid):
        raise ValueError("SSID must be 1–32 UTF-8 bytes without control characters.")
    address = ipaddress.ip_interface(args.address)
    if address.version != 4 or address.network.prefixlen != 24 or not any(address.network.subnet_of(net) for net in PRIVATE):
        raise ValueError("Use a private IPv4 /24, such as 10.77.0.1/24.")
    if address.ip in (address.network.network_address, address.network.broadcast_address):
        raise ValueError("The AP address must be a usable host address.")
    if NAME in output("nmcli", "-g", "NAME", "connection", "show").splitlines():
        raise ValueError(f"An unrelated connection named {NAME} exists; refusing to replace it.")
    if output("nmcli", "-g", "GENERAL.TYPE", "device", "show", args.wifi) != "wifi":
        raise ValueError("The Wi-Fi interface must be managed by NetworkManager.")
    if output("nmcli", "-g", "WIFI-PROPERTIES.AP", "device", "show", args.wifi) != "yes":
        raise ValueError("This adapter/driver does not advertise access-point support.")
    if not output("nmcli", "-g", "GENERAL.STATE", "device", "show", args.wifi).startswith("30 "):
        raise ValueError("Wi-Fi must be disconnected and available; existing connections will not be replaced.")
    links = json.loads(output("ip", "-j", "link", "show", "dev", args.uplink))
    if not links or "UP" not in links[0]["flags"]:
        raise ValueError("The Ethernet uplink is not up.")
    routes = json.loads(output("ip", "-j", "-4", "route", "show", "table", "all"))
    blocked = set(BLOCKED)
    # shortcut: public corporate routes need --block-cidr; use managed network policy before production.
    blocked.update(str(ipaddress.IPv4Network(value)) for value in args.block_cidr)
    for route in routes:
        destination = route.get("dst", "default")
        if destination == "default":
            continue
        network = ipaddress.ip_network(destination, strict=False)
        # Full-tunnel VPN default halves are not local subnet conflicts.
        if network.prefixlen > 1 and network.overlaps(address.network):
            raise ValueError(f"AP subnet overlaps {network} on {route.get('dev', '?')}; choose another --address.")
        if route.get("scope") == "link" and network.prefixlen > 1:
            blocked.add(str(network))
    config = dict(
        uuid=str(uuid.uuid4()), wifi=args.wifi, uplink=args.uplink, address=str(address),
        blocked=sorted(blocked), band={"2.4ghz": "bg", "5ghz": "a"}[args.band],
    )
    for binary, chain, _, _ in firewall(config):
        if rule_exists(binary, "-w", "5", "-S", chain):
            raise ValueError(f"Firewall chain {chain} already exists; refusing to replace it.")
    return config


def stop(config):
    active = output("nmcli", "-g", "UUID", "connection", "show", "--active").splitlines()
    if config["uuid"] in active:
        run("nmcli", "--wait", "20", "connection", "down", "uuid", config["uuid"])
    if config["uuid"] in output("nmcli", "-g", "UUID", "connection", "show", "--active").splitlines():
        raise RuntimeError("Hotspot is still active; keeping its firewall and state. Retry 'down'.")
    if config["uuid"] in output("nmcli", "-g", "UUID", "connection", "show").splitlines():
        run("nmcli", "connection", "delete", "uuid", config["uuid"])
    PROFILE.unlink(missing_ok=True)
    # Never withdraw guards until NetworkManager confirms the AP has stopped.
    remove_firewall(config)
    (STATE / "state.json").unlink(missing_ok=True)
    STATE.rmdir()


def start(args):
    config = prepare(args)
    password = getpass.getpass("Wi-Fi password (8–63 printable ASCII characters): ")
    if not 8 <= len(password) <= 63 or any(not 32 <= ord(c) <= 126 for c in password):
        raise ValueError("Password must be 8–63 printable ASCII characters.")
    if password != getpass.getpass("Confirm Wi-Fi password: "):
        raise ValueError("Passwords do not match.")
    STATE.mkdir(mode=0o700)
    try:
        private_file(STATE / "state.json", json.dumps(config))
    except BaseException:
        (STATE / "state.json").unlink(missing_ok=True)
        STATE.rmdir()
        raise
    try:
        install_firewall(config)
        PROFILE.parent.mkdir(parents=True, exist_ok=True)
        private_file(PROFILE, profile(config, args.ssid, password))
        run("nmcli", "connection", "load", str(PROFILE))
        run("nmcli", "--wait", "30", "connection", "up", "uuid", config["uuid"])
    except BaseException:
        try:
            stop(config)
        except (OSError, RuntimeError, subprocess.SubprocessError) as error:
            print(f"Cleanup incomplete; guards/state retained where possible. Retry 'down': {error}", file=sys.stderr)
        raise
    print(f"Hidden WPA2 hotspot {args.ssid!r} is up on {args.wifi} ({args.band}); provider IP: {config['address']}.")
    print("Join from iPhone Settings → Wi-Fi → Other… → WPA2; enter the SSID and password.")
    print(f"Find its address with: ip neigh show dev {args.wifi}")
    print("Temporary profile: no autostart; run this script with 'down' before reboot/firewall maintenance.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    up = actions.add_parser("up", help="Create a temporary hidden WPA2 hotspot; prompts for password")
    up.add_argument("--wifi", required=True, help="Unused NetworkManager Wi-Fi interface, e.g. wlan0")
    up.add_argument("--uplink", required=True, help="Internet-facing Ethernet interface, e.g. eth0")
    up.add_argument("--ssid", default="YARD-Test")
    up.add_argument("--band", choices=("2.4ghz", "5ghz"), default="2.4ghz", help="AP frequency band; requires adapter/driver AP support (default: 2.4ghz)")
    up.add_argument("--address", default="10.77.0.1/24", help="Provider's private /24 AP address")
    up.add_argument("--block-cidr", action="append", default=[], help="Additional company IPv4 network to block; repeatable")
    actions.add_parser("down", help="Stop this script's hotspot and remove its runtime profile/firewall rules")
    args = parser.parse_args()
    if sys.platform != "linux" or os.geteuid() != 0:
        parser.error("Run on the Linux provider host with sudo, outside Docker.")
    dependencies = ["nmcli", "iptables", "ip6tables"] + (["ip", "dnsmasq"] if args.action == "up" else [])
    missing = [name for name in dependencies if shutil.which(name) is None]
    if missing:
        parser.error(f"Missing host tools: {', '.join(missing)}. Install these first; no packages/services are changed automatically.")
    try:
        if args.action == "up":
            start(args)
        elif (STATE / "state.json").exists():
            stop(json.loads((STATE / "state.json").read_text()))
            print("Hotspot stopped; its runtime profile and firewall rules removed.")
        else:
            print("No hotspot state found; nothing changed.")
    except KeyboardInterrupt:
        print("Interrupted. If cleanup was incomplete, retry 'down'.", file=sys.stderr)
        return 130
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Error: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
