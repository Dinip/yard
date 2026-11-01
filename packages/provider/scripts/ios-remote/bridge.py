#!/usr/bin/env python3
"""YARD - Device Farm: relay a paired iPhone's CoreDevice traffic over a lab VPN."""

import argparse
import asyncio
import contextlib
import ipaddress
import json
from pathlib import Path
import re
import resource
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time


CONFIG = Path(__file__).resolve().parent / "state/phone.json"
SERVICE = "_remotepairing._tcp"


def endpoint_ip(value):
    address = ipaddress.IPv4Address(value)
    if address.is_unspecified or address.is_multicast or int(address) == 0xFFFFFFFF:
        raise ValueError("Use a specific unicast IPv4 address.")
    return str(address)


def port_range(value):
    match = re.fullmatch(r"(\d+)(?:-(\d+))?", value)
    if not match:
        raise ValueError("Ports must be a number or a range, such as 55000-55400.")
    low, high = int(match[1]), int(match[2] or match[1])
    if not 1024 <= low <= high <= 65535:
        raise ValueError("Ports must be an increasing range within 1024-65535.")
    return range(low, high + 1)


def dns_query(*args):
    return subprocess.run(
        ["/usr/bin/dns-sd", "-t", "8", *args], check=True,
        capture_output=True, text=True, timeout=12,
    ).stdout


def parse_resolve(instance, output):
    match = re.search(r"can be reached at (\S+):(\d+) \(interface \d+\)[^\n]*\n([^\n]+)", output)
    if not match:
        raise ValueError("Could not resolve the iPhone's hostname and TXT record. Retry capture.")
    record = dict(instance=instance, host=match[1], port=int(match[2]), txt=shlex.split(match[3]))
    validate_record(record)
    return record


def validate_record(record):
    if not isinstance(record, dict) or not all(key in record for key in ("instance", "host", "port", "txt")):
        raise ValueError("Invalid saved Bonjour record. Run capture again.")
    if not isinstance(record["instance"], str) or not 1 <= len(record["instance"].encode()) <= 63:
        raise ValueError("Invalid Bonjour instance name.")
    if not isinstance(record["host"], str) or not re.fullmatch(r"\S+\.local\.?", record["host"]):
        raise ValueError("Invalid Bonjour .local hostname.")
    if type(record["port"]) is not int or not 1024 <= record["port"] <= 65535:
        raise ValueError("Invalid RemotePairing port.")
    txt = record["txt"]
    if not isinstance(txt, list) or not all(isinstance(item, str) and "\0" not in item for item in txt):
        raise ValueError("Invalid Bonjour TXT record.")
    if not {"identifier", "authTag"} <= {item.split("=", 1)[0] for item in txt}:
        raise ValueError("Bonjour TXT record lacks identifier/authTag. Run capture again.")
    if any(not 1 <= len(item.encode()) <= 255 for item in txt):
        raise ValueError("Each Bonjour TXT entry must be within 1-255 bytes.")


def save_record(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Replace atomically so a failed refresh preserves the last working record.
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(data, file, indent=2)
            file.write("\n")
            file.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


async def relay_tcp(reader, writer, remote, allowed_ips):
    upstream = None
    tasks = []
    try:
        client_ip = writer.get_extra_info("peername")[0]
        if client_ip not in allowed_ips:
            print(f"Rejected TCP source {client_ip}; check --allow on this relay.", file=sys.stderr, flush=True)
            return
        other_reader, upstream = await asyncio.wait_for(asyncio.open_connection(*remote), 10)

        async def copy(source, destination):
            while data := await source.read(65536):
                destination.write(data)
                await destination.drain()
            if destination.can_write_eof():
                destination.write_eof()

        tasks = [asyncio.create_task(copy(reader, upstream)), asyncio.create_task(copy(other_reader, writer))]
        await asyncio.gather(*tasks)
    except (OSError, TimeoutError) as error:
        print(f"TCP {remote[1]}: {error}", file=sys.stderr)
    finally:
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        for stream in (writer, upstream):
            if stream:
                stream.close()
                with contextlib.suppress(OSError):
                    await stream.wait_closed()


class UdpRelay(asyncio.DatagramProtocol):
    def __init__(self, remote, allowed_ips):
        self.remote, self.allowed_ips = remote, allowed_ips
        self.loop = asyncio.get_running_loop()
        self.flows = {}

    def connection_made(self, transport):
        self.transport = transport

    def datagram_received(self, data, client):
        if client[0] not in self.allowed_ips:
            return
        try:
            if client not in self.flows:
                upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                try:
                    upstream.setblocking(False)
                    upstream.connect(self.remote)
                    self.loop.add_reader(upstream.fileno(), self.reply, client)
                except BaseException:
                    upstream.close()
                    raise
                self.flows[client] = [upstream, time.monotonic()]
            upstream, _ = self.flows[client]
            self.flows[client][1] = time.monotonic()
            upstream.send(data)
        except OSError:
            self.drop(client)

    def reply(self, client):
        upstream, _ = self.flows[client]
        try:
            self.transport.sendto(upstream.recv(65535), client)
            self.flows[client][1] = time.monotonic()
        except BlockingIOError:
            pass
        except OSError:
            self.drop(client)

    def drop(self, client):
        flow = self.flows.pop(client, None)
        if flow:
            self.loop.remove_reader(flow[0].fileno())
            flow[0].close()

    def expire(self):
        for client, (_, last_used) in list(self.flows.items()):
            if time.monotonic() - last_used > 300:
                self.drop(client)

    def connection_lost(self, error):
        for client in list(self.flows):
            self.drop(client)


# shortcut: this lab relay checks source IPs only; add reservation auth before shared use.
async def connect(args):
    if args.timeout <= 0:
        raise ValueError("--timeout must be a positive number of seconds.")
    is_relay = args.command == "relay"
    if is_relay:
        if not args.bind or not args.allow or args.via:
            raise ValueError("relay requires --bind SERVER_IPV4 and --allow CLIENT_IPV4; --via is only for connect.")
        bind_ip = endpoint_ip(args.bind)
        allowed_ips = {endpoint_ip(value) for value in args.allow}
    else:
        if args.bind or args.allow:
            raise ValueError("--bind and --allow are only for relay mode.")
        if not args.via:
            raise ValueError("connect requires --via with the Linux relay's IPv4 address.")
        socket.if_nametoindex(args.interface)
        bind_ip = endpoint_ip(subprocess.check_output(["/usr/sbin/ipconfig", "getifaddr", args.interface], text=True).strip())
        allowed_ips = {bind_ip}
    config = json.loads(args.config.read_text())
    iphone = endpoint_ip(args.iphone or config["iphone"])
    target = endpoint_ip(args.via) if args.via else iphone
    record = config["record"]
    validate_record(record)
    # shortcut: iOS tunnel ports are observed, not guaranteed; widen --ports if Apple changes them.
    ports = sorted({record["port"], *port_range(args.ports)})
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    needed = len(ports) * 2 + 256
    if hard != resource.RLIM_INFINITY and needed > hard:
        raise ValueError(f"This port range needs at least {needed} file descriptors; the hard limit is {hard}.")
    if soft != resource.RLIM_INFINITY and soft < needed:
        resource.setrlimit(resource.RLIMIT_NOFILE, (needed, hard))
    try:
        _, writer = await asyncio.wait_for(asyncio.open_connection(target, record["port"]), 5)
        writer.close()
        await writer.wait_closed()
    except (OSError, TimeoutError) as error:
        raise ValueError(f"RemotePairing port at {target}:{record['port']} is unreachable. Check routing, Wi-Fi, and relay/firewall access.") from error

    servers, transports, relays = [], [], []
    connections = set()

    def accept(reader, writer, remote):
        task = asyncio.create_task(relay_tcp(reader, writer, remote, allowed_ips))
        connections.add(task)
        task.add_done_callback(connections.discard)

    registration = None
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    lease = loop.call_later(args.timeout, stop.set)
    try:
        for port in ports:
            remote = (target, port)
            server = await asyncio.start_server(
                lambda r, w, remote=remote: accept(r, w, remote), bind_ip, port,
            )
            servers.append(server)
            transport, relay = await loop.create_datagram_endpoint(
                lambda remote=remote: UdpRelay(remote, allowed_ips), local_addr=(bind_ip, port),
            )
            transports.append(transport)
            relays.append(relay)
        if not is_relay:
            # A separate hostname prevents cached iPhone AAAA records from bypassing the relay.
            proxy_hostname = f"yard-ios-{bind_ip.replace('.', '-')}.local."
            registration = await asyncio.create_subprocess_exec(
                "/usr/bin/dns-sd", "-i", args.interface, "-P", record["instance"], SERVICE,
                "local.", str(record["port"]), proxy_hostname, bind_ip, *record["txt"],
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
            )

            async def registered():
                while line := await registration.stdout.readline():
                    if b"Name now registered and active" in line:
                        return
                    if b"Error" in line or b"error" in line or b"Name in use" in line:
                        break
                raise ValueError("Bonjour registration failed. Disconnect local iPhone discovery and stop any other bridge, then retry.")

            await asyncio.wait_for(registered(), 15)
        print(f"{args.command} listening on {bind_ip}, forwarding to {target}.", flush=True)
        print(f"TCP/UDP ports: {record['port']} and {args.ports}. Allowed source IPv4 addresses: {', '.join(sorted(allowed_ips))}.")
        if is_relay:
            print("No Bonjour advertisement on the server. Run connect --via on each developer Mac.")
        else:
            print("Check Xcode or run: xcrun devicectl list devices")
            print("Then request a live service: xcrun devicectl device info apps --device <DEVICE_IDENTIFIER> --timeout 20")
        print("Listeners are ready; an end-to-end CoreDevice connection is still required. Ctrl-C stops the bridge.", flush=True)
        print(f"Lab PoC only: no YARD - Device Farm reservation enforcement. Stops after {args.timeout}s.", flush=True)
        while not stop.is_set():
            try:
                await asyncio.wait_for(stop.wait(), 5)
            except TimeoutError:
                pass
            if registration and registration.returncode is not None:
                raise ValueError("Bonjour registration exited; restart the bridge.")
            if not is_relay:
                current_ip = subprocess.run(
                    ["/usr/sbin/ipconfig", "getifaddr", args.interface], capture_output=True, text=True,
                ).stdout.strip()
                if current_ip != bind_ip:
                    raise ValueError("Mac LAN address changed; restart the bridge.")
            for relay in relays:
                relay.expire()
    finally:
        lease.cancel()
        if registration:
            if registration.returncode is None:
                registration.terminate()
            await registration.wait()
        for server in servers:
            server.close()
        pending = list(connections)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        await asyncio.gather(*(server.wait_closed() for server in servers))
        for transport in transports:
            transport.close()
        for relay in relays:
            relay.connection_lost(None)
        print("Bridge stopped; advertisements and relay sockets removed.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("connect", "relay"))
    parser.add_argument("--config", type=Path, default=CONFIG)
    parser.add_argument("--iphone", help="iPhone Wi-Fi IPv4 address; overrides the saved config")
    parser.add_argument("--via", help="connect through this relay server's IPv4 address")
    parser.add_argument("--bind", help="relay server's local IPv4 address to listen on; required for relay")
    parser.add_argument("--allow", action="append", default=[], help="allowed source IPv4 as seen by relay server; repeat for each client")
    parser.add_argument("--interface", default="en0", help="Mac LAN interface, default en0")
    parser.add_argument("--ports", default="62000-62300", help="Observed tunnel range, default 62000-62300; must match at both hops")
    parser.add_argument("--timeout", type=int, default=1800, help="stop connect/relay after this many seconds (default 1800)")
    args = parser.parse_args()
    try:
        if sys.platform != "darwin" and args.command != "relay":
            raise ValueError("connect requires macOS; relay also supports Linux.")
        asyncio.run(connect(args))
    except FileNotFoundError:
        parser.exit(1, "Missing config or macOS tool. Capture state/phone.json on Linux with pairing.py capture-device; see README.md.\n")
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, TimeoutError) as error:
        parser.exit(1, f"Error: {error}\n")
    except KeyboardInterrupt:
        parser.exit(130, "Stopped.\n")


if __name__ == "__main__":
    main()
