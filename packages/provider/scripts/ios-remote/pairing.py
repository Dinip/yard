#!/usr/bin/env python3
"""YARD - Device Farm: relay native Device Hub pairing through a Linux host."""

import argparse
import asyncio
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import re
import signal
import socket
import sys
import time

import bridge

SERVICE = "_remotepairing-pairable-host._tcp"


def capture(args):
    output = bridge.dns_query("-m", "-i", args.interface, "-B", SERVICE, "local.")
    found = re.findall(r"\bAdd\s+\S+\s+(\d+)\s+local\.\s+_remotepairing-pairable-host\._tcp\.\s+(.+)", output)
    candidates = {name.strip(): interface for interface, name in found}
    if args.instance:
        candidates = {name: interface for name, interface in candidates.items() if name == args.instance}
    if len(candidates) != 1:
        raise ValueError("Open Device Hub > Pair Nearby Device. If several hosts appear, use --instance with one of: " + ", ".join(candidates))
    name, interface = next(iter(candidates.items()))
    record = bridge.parse_resolve(name, bridge.dns_query("-m", "-i", interface, "-L", name, SERVICE, "local."))
    bridge.save_record(args.record, record)
    print(f"Saved native Device Hub advertisement to {args.record} with owner-only permissions.")
    print(f"Device Hub TCP port: {record['port']}. Recapture if the pairing dialog is reopened.")


def capture_device(args):
    if not args.iphone:
        raise ValueError("capture-device requires --iphone with the phone's Wi-Fi IPv4 address.")
    phone = bridge.endpoint_ip(args.iphone)
    client = Advertisement()
    try:
        records = {
            record["instance"]: record
            for record, address in client.discover(args.interface, args.timeout or 8)
            if address == phone and (not args.instance or record["instance"] == args.instance)
        }
        if not records:
            raise ValueError("No matching phone advertisement found. Check the phone's Wi-Fi, IP, Developer Mode, and capture interface.")
        if len(records) != 1:
            raise ValueError("Multiple instances at this phone IP. Retry with --instance and one of: " + ", ".join(sorted(records)))
        record = next(iter(records.values()))
        bridge.validate_record(record)
        bridge.save_record(args.record, dict(iphone=phone, record=record))
        print(f"Captured the phone at {phone}, instance {record['instance']}, port {record['port']}.")
        print(f"Saved owner-only discovery config to {args.record}; no pairing private keys were read.")
    finally:
        client.close()


class BusError(ctypes.Structure):
    _fields_ = [("name", ctypes.c_char_p), ("message", ctypes.c_char_p), ("need_free", ctypes.c_int)]


class Advertisement:
    """Keep one Avahi entry group alive through the installed system D-Bus API."""

    def __init__(self):
        library = ctypes.util.find_library("systemd")
        if not library:
            raise ValueError("This experiment requires Linux with libsystemd and avahi-daemon.")
        self.lib = ctypes.CDLL(library)
        ptr = ctypes.c_void_p
        self.lib.sd_bus_open_system.argtypes = [ctypes.POINTER(ptr)]
        self.lib.sd_bus_call_method.argtypes = [ptr, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.POINTER(BusError), ctypes.POINTER(ptr), ctypes.c_char_p]
        self.lib.sd_bus_message_read.argtypes = [ptr, ctypes.c_char_p]
        self.lib.sd_bus_message_unref.argtypes = [ptr]
        self.lib.sd_bus_message_unref.restype = ptr
        self.lib.sd_bus_error_free.argtypes = [ctypes.POINTER(BusError)]
        self.lib.sd_bus_flush_close_unref.argtypes = [ptr]
        self.lib.sd_bus_flush_close_unref.restype = ptr
        self.bus = ptr()
        self.callbacks = []
        result = self.lib.sd_bus_open_system(ctypes.byref(self.bus))
        if result < 0:
            raise OSError(-result, "Cannot connect to the system D-Bus")
        self.group = None
        try:
            self.group = self.call("/", "org.freedesktop.Avahi.Server", "EntryGroupNew", result_type=b"o")
        except BaseException:
            self.close()
            raise

    def call(self, path, interface, method, signature=b"", *args, result_type=None):
        reply = ctypes.c_void_p()
        error = BusError()
        try:
            result = self.lib.sd_bus_call_method(
                self.bus, b"org.freedesktop.Avahi", path.encode(), interface.encode(), method.encode(),
                ctypes.byref(error), ctypes.byref(reply), signature, *args,
            )
            if result < 0:
                message = error.message.decode(errors="replace") if error.message else os.strerror(-result)
                raise OSError(-result, f"Avahi {method}: {message}")
            if result_type:
                if result_type == b"service":
                    return self.read_service(reply)
                value = ctypes.c_char_p() if result_type == b"o" else ctypes.c_int()
                result = self.lib.sd_bus_message_read(reply, result_type, ctypes.byref(value))
                if result <= 0:
                    raise ValueError(f"Invalid Avahi {method} reply.")
                return value.value.decode() if result_type == b"o" else value.value
        finally:
            self.lib.sd_bus_message_unref(reply)
            self.lib.sd_bus_error_free(ctypes.byref(error))

    def read_service(self, reply):
        fields = [ctypes.c_int(), ctypes.c_int(), ctypes.c_char_p(), ctypes.c_char_p(),
                  ctypes.c_char_p(), ctypes.c_char_p(), ctypes.c_int(), ctypes.c_char_p(), ctypes.c_uint16()]
        if self.lib.sd_bus_message_read(reply, b"iissssisq", *(ctypes.byref(value) for value in fields)) <= 0:
            raise ValueError("Invalid Avahi service reply.")
        ptr = ctypes.c_void_p
        self.lib.sd_bus_message_enter_container.argtypes = [ptr, ctypes.c_char, ctypes.c_char_p]
        self.lib.sd_bus_message_read_array.argtypes = [ptr, ctypes.c_char, ctypes.POINTER(ptr), ctypes.POINTER(ctypes.c_size_t)]
        self.lib.sd_bus_message_exit_container.argtypes = [ptr]
        if self.lib.sd_bus_message_enter_container(reply, b"a", b"ay") <= 0:
            raise ValueError("Missing Avahi TXT array.")
        txt = []
        while True:
            data, size = ptr(), ctypes.c_size_t()
            result = self.lib.sd_bus_message_read_array(reply, b"y", ctypes.byref(data), ctypes.byref(size))
            if result < 0:
                raise ValueError("Invalid Avahi TXT array.")
            if result == 0:
                break
            raw = ctypes.string_at(data, size.value)
            txt.append("".join(chr(byte) if 32 <= byte < 127 and byte != 92 else f"\\x{byte:02x}" for byte in raw))
        if self.lib.sd_bus_message_exit_container(reply) < 0:
            raise ValueError("Invalid Avahi TXT array end.")
        record = dict(instance=fields[2].value.decode(), host=fields[5].value.decode(), port=fields[8].value, txt=txt)
        return record, fields[7].value.decode()

    def discover(self, interface, timeout):
        ptr = ctypes.c_void_p
        callback_type = ctypes.CFUNCTYPE(ctypes.c_int, ptr, ptr, ctypes.POINTER(BusError))
        self.lib.sd_bus_add_match.argtypes = [ptr, ctypes.POINTER(ptr), ctypes.c_char_p, callback_type, ptr]
        self.lib.sd_bus_process.argtypes = [ptr, ctypes.POINTER(ptr)]
        self.lib.sd_bus_wait.argtypes = [ptr, ctypes.c_uint64]
        names = set()

        def item_new(message, userdata, error):
            values = [ctypes.c_int(), ctypes.c_int(), ctypes.c_char_p(), ctypes.c_char_p(), ctypes.c_char_p(), ctypes.c_uint()]
            result = self.lib.sd_bus_message_read(message, b"iisssu", *(ctypes.byref(value) for value in values))
            if result <= 0:
                return -74
            if values[3].value == b"_remotepairing._tcp" and values[4].value == b"local":
                names.add(values[2].value.decode())
            return 1

        callback = callback_type(item_new)
        self.callbacks.append(callback)
        match = b"type='signal',sender='org.freedesktop.Avahi',interface='org.freedesktop.Avahi.ServiceBrowser',member='ItemNew'"
        if self.lib.sd_bus_add_match(self.bus, None, match, callback, None) < 0:
            raise ValueError("Cannot subscribe to Avahi discovery.")
        index = socket.if_nametoindex(interface)
        self.call("/", "org.freedesktop.Avahi.Server", "ServiceBrowserNew", b"iissu",
                  ctypes.c_int(index), ctypes.c_int(-1), b"_remotepairing._tcp", b"local", ctypes.c_uint(0), result_type=b"o")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = self.lib.sd_bus_process(self.bus, None)
            if result < 0:
                raise OSError(-result, "Avahi discovery failed")
            if result == 0:
                self.lib.sd_bus_wait(self.bus, ctypes.c_uint64(min(500000, max(0, int((deadline - time.monotonic()) * 1000000)))))
        records = []
        for name in sorted(names):
            try:
                records.append(self.call("/", "org.freedesktop.Avahi.Server", "ResolveService", b"iisssiu",
                                         ctypes.c_int(index), ctypes.c_int(-1), name.encode(), b"_remotepairing._tcp", b"local", ctypes.c_int(0), ctypes.c_uint(0), result_type=b"service"))
            except OSError:
                continue  # An instance may rotate between browsing and resolving.
        return records

    def publish(self, record, bind_ip, port, interface):
        host = f"devicehub-relay-{bind_ip.replace('.', '-')}.local"
        index = socket.if_nametoindex(interface)
        entry = "org.freedesktop.Avahi.EntryGroup"
        self.call(self.group, entry, "AddAddress", b"iiuss", ctypes.c_int(index), ctypes.c_int(0), ctypes.c_uint(0), host.encode(), bind_ip.encode())
        # Preserve the identity and authentication tag; only mark the discovery display name.
        txt = [value + " via YARD - Device Farm" if value.startswith("name=") else value for value in record["txt"]]
        fields = [ctypes.c_int(index), ctypes.c_int(0), ctypes.c_uint(0),
                  (record["instance"] + "-relay").encode(), SERVICE.encode(), b"local", host.encode(),
                  ctypes.c_int(port), ctypes.c_int(len(txt))]
        for value in txt:
            data = value.encode()
            if not 1 <= len(data) <= 255:
                raise ValueError("Invalid pairing TXT record size.")
            fields.append(ctypes.c_int(len(data)))
            fields.extend(ctypes.c_int(byte) for byte in data)
        self.call(self.group, entry, "AddService", b"iiussssqaay", *fields)
        self.call(self.group, entry, "Commit")

    def state(self):
        return self.call(self.group, "org.freedesktop.Avahi.EntryGroup", "GetState", result_type=b"i")

    def close(self):
        # Closing this connection also withdraws its Avahi entry group.
        if self.bus:
            self.lib.sd_bus_flush_close_unref(self.bus)
            self.bus = None
            self.callbacks.clear()


# shortcut: pairing has a manual lab lifetime; bind it to a reservation before shared use.
async def run(args):
    bind_ip = bridge.endpoint_ip(args.bind)
    phone = bridge.endpoint_ip(args.allow)
    record = json.loads(args.record.read_text())
    bridge.validate_record(record)
    if len((record["instance"] + "-relay").encode()) > 63:
        raise ValueError("Pairing service name is too long.")
    listen_port = args.listen if args.listen is not None else record["port"]
    for port in (listen_port, args.forward):
        if not 1024 <= port <= 65535:
            raise ValueError("Ports must be within 1024-65535.")
    # Do not probe Device Hub: even an empty TCP connection can consume a pairing attempt.
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    connections = set()

    async def handle(reader, writer):
        if writer.get_extra_info("peername")[0] == phone:
            print(f"iPhone connected to pairing relay at {bind_ip}:{listen_port}.", flush=True)
        await bridge.relay_tcp(reader, writer, ("127.0.0.1", args.forward), {phone})

    def accept(reader, writer):
        task = asyncio.create_task(handle(reader, writer))
        connections.add(task)
        task.add_done_callback(connections.discard)

    server = await asyncio.start_server(accept, bind_ip, listen_port)
    advertisement = None
    try:
        advertisement = Advertisement()
        advertisement.publish(record, bind_ip, listen_port, args.interface)
        for _ in range(50):
            state = advertisement.state()
            if state == 2:
                break
            if state in (3, 4):
                raise ValueError("Avahi could not publish the pairing service.")
            await asyncio.sleep(.1)
        else:
            raise ValueError("Timed out publishing the pairing service.")
        print(f"Pairing service advertised on {args.interface} at {bind_ip}:{listen_port}.", flush=True)
        print("Look for the Mac's name followed by 'via YARD - Device Farm' on the iPhone.", flush=True)
        print("Device Hub handles trust and PIN entry. This relay only copies bytes. Ctrl-C withdraws the advertisement.", flush=True)
        print(f"Lab PoC only: no reservation enforcement. Stops after {args.timeout or 600}s.", flush=True)
        try:
            await asyncio.wait_for(stop.wait(), args.timeout or 600)
        except TimeoutError:
            print("Pairing experiment timed out.", flush=True)
    finally:
        if advertisement:
            advertisement.close()
        server.close()
        pending = list(connections)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pending, return_exceptions=True)
        await server.wait_closed()
        print("Pairing relay stopped; advertisement withdrawn.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("capture", "capture-device", "serve"))
    parser.add_argument("--record", type=Path, required=True, help="private JSON capture/config path")
    parser.add_argument("--instance", help="native Device Hub service instance to capture")
    parser.add_argument("--iphone", help="Wi-Fi IPv4 of the phone to capture on Linux")
    parser.add_argument("--bind", help="Linux server IPv4 reachable by the iPhone")
    parser.add_argument("--allow", help="only this iPhone source IPv4 may connect")
    parser.add_argument("--interface", required=True, help="Linux interface on the iPhone's network")
    parser.add_argument("--forward", type=int, help="loopback port opened by SSH -R")
    parser.add_argument("--listen", type=int, help="public pairing relay port; defaults to captured host port")
    parser.add_argument("--timeout", type=int, help="serve lifetime, default 600; capture-device browse time, default 8")
    args = parser.parse_args()
    try:
        if args.command == "capture":
            if sys.platform != "darwin":
                raise ValueError("Capture the native Device Hub advertisement on the developer Mac.")
            capture(args)
            return
        if sys.platform != "linux":
            raise ValueError("Run this pairing relay on Linux; keep native Device Hub on the developer Mac.")
        if args.timeout is not None and args.timeout <= 0:
            raise ValueError("--timeout must be positive.")
        if args.command == "capture-device":
            capture_device(args)
            return
        if not args.bind or not args.allow or args.forward is None:
            raise ValueError("serve requires --bind, --allow, and --forward.")
        asyncio.run(run(args))
    except (OSError, ValueError, TypeError, KeyError, bridge.subprocess.SubprocessError, TimeoutError) as error:
        parser.exit(1, f"Error: {error}\n")
    except KeyboardInterrupt:
        parser.exit(130, "Stopped.\n")


if __name__ == "__main__":
    main()
