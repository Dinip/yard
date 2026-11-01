import asyncio
from pathlib import Path
import socket
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, Mock, patch

import bridge


class MetadataTests(unittest.TestCase):
    def test_private_record_refresh_is_atomic(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state/phone.json"
            bridge.save_record(path, {"version": 1})
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            with patch.object(bridge.json, "dump", side_effect=OSError("disk full")):
                with self.assertRaisesRegex(OSError, "disk full"):
                    bridge.save_record(path, {"version": 2})
            self.assertEqual(bridge.json.loads(path.read_text()), {"version": 1})
            self.assertEqual(list(path.parent.iterdir()), [path])

    def test_resolve_preserves_txt_arguments_without_executing_them(self):
        output = r"""12:00:00.000 UUID._remotepairing._tcp.local. can be reached at Test-iPhone.local.:49152 (interface 14)
 identifier=UUID authTag=ab\+cd\= ver=24 minVer=8 flags=0 note=hello\ world literal=\\x01
"""
        record = bridge.parse_resolve("UUID", output)
        self.assertEqual(record["host"], "Test-iPhone.local.")
        self.assertEqual(record["port"], 49152)
        self.assertEqual(record["txt"], ["identifier=UUID", "authTag=ab+cd=", "ver=24", "minVer=8", "flags=0", "note=hello world", r"literal=\x01"])

    def test_invalid_inputs(self):
        for value in ("0.0.0.0", "224.0.0.251", "255.255.255.255", "example.com", "::1"):
            with self.subTest(ip=value), self.assertRaises(ValueError):
                bridge.endpoint_ip(value)
        for value in ("100.75.133.19", "10.20.30.40", "192.168.1.1", "127.0.0.1"):
            self.assertEqual(bridge.endpoint_ip(value), value)
        for value in ("0-5", "55400-55000", "55000;echo nope", "65536", "-1"):
            with self.subTest(ports=value), self.assertRaises(ValueError):
                bridge.port_range(value)
        self.assertEqual(list(bridge.port_range("55000-55002")), [55000, 55001, 55002])
        with self.assertRaises(ValueError):
            bridge.parse_resolve("UUID", "No Such Record")
        with self.assertRaises(ValueError):
            bridge.parse_resolve("UUID", "can be reached at Test.local.:49152 (interface 1)\n ver=24\n")


class RelayTests(unittest.IsolatedAsyncioTestCase):
    async def test_mac_connection_requires_the_relay(self):
        with self.assertRaisesRegex(ValueError, "requires --via"):
            await bridge.connect(SimpleNamespace(
                command="connect", bind=None, allow=[], via=None, timeout=1800,
            ))

    async def test_nonpositive_lease_is_rejected_before_opening_sockets(self):
        for seconds in (0, -1):
            with self.subTest(seconds=seconds), self.assertRaises(ValueError):
                await bridge.connect(SimpleNamespace(timeout=seconds))

    async def test_lease_expiry_closes_relay_resources(self):
        record = dict(instance="UUID", host="Test.local.", port=49152, txt=["identifier=UUID", "authTag=test"])
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "phone.json"
            bridge.save_record(config, dict(iphone="10.20.0.5", record=record))
            args = SimpleNamespace(
                command="relay", config=config, iphone=None, ports="55000", via=None,
                bind="127.0.0.1", allow=["127.0.0.1"], timeout=0.01,
            )
            writer = Mock(wait_closed=AsyncMock())
            server = Mock(wait_closed=AsyncMock())
            transport, relay = Mock(), Mock()
            loop = asyncio.get_running_loop()
            with (
                patch.object(loop, "add_signal_handler"),
                patch.object(bridge.resource, "setrlimit"),
                patch.object(bridge.asyncio, "open_connection", new=AsyncMock(return_value=(None, writer))),
                patch.object(bridge.asyncio, "start_server", new=AsyncMock(return_value=server)),
                patch.object(loop, "create_datagram_endpoint", new=AsyncMock(return_value=(transport, relay))),
            ):
                await asyncio.wait_for(bridge.connect(args), 2)
            self.assertEqual(server.close.call_count, 2)
            self.assertEqual(transport.close.call_count, 2)
            self.assertEqual(relay.connection_lost.call_count, 2)

    async def test_partial_startup_failure_closes_existing_listeners(self):
        record = dict(instance="UUID", host="Test.local.", port=49152, txt=["identifier=UUID", "authTag=test"])
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.json"
            config.write_text(bridge.json.dumps(dict(iphone="100.75.133.19", record=record)))
            args = SimpleNamespace(command="connect", config=config, iphone=None, interface="lo0", ports="55000", via="10.30.0.1", bind=None, allow=[], timeout=1800)
            writer = Mock(wait_closed=AsyncMock())
            server = Mock(wait_closed=AsyncMock())
            loop = asyncio.get_running_loop()
            with (
                patch.object(bridge.subprocess, "check_output", return_value="127.0.0.1"),
                patch.object(bridge.socket, "if_nametoindex", return_value=1),
                patch.object(bridge.resource, "setrlimit"),
                patch.object(loop, "add_signal_handler"),
                patch.object(bridge.asyncio, "open_connection", new=AsyncMock(return_value=(None, writer))),
                patch.object(bridge.asyncio, "start_server", new=AsyncMock(return_value=server)),
                patch.object(loop, "create_datagram_endpoint", new=AsyncMock(side_effect=OSError("port occupied"))),
            ):
                with self.assertRaisesRegex(OSError, "port occupied"):
                    await bridge.connect(args)
            server.close.assert_called_once()
            server.wait_closed.assert_awaited_once()

    async def test_two_hop_tcp_backpressure_and_half_close(self):
        async def echo(reader, writer):
            data = await reader.read()
            writer.write(data[::-1])
            await writer.drain()
            writer.close()
            await writer.wait_closed()

        upstream = await asyncio.start_server(echo, "127.0.0.1", 0)
        remote = upstream.sockets[0].getsockname()
        server_proxy = await asyncio.start_server(
            lambda r, w: bridge.relay_tcp(r, w, remote, {"127.0.0.1"}), "127.0.0.1", 0,
        )
        server_address = server_proxy.sockets[0].getsockname()
        proxy = await asyncio.start_server(
            lambda r, w: bridge.relay_tcp(r, w, server_address, {"127.0.0.1"}), "127.0.0.1", 0,
        )
        try:
            reader, writer = await asyncio.open_connection(*proxy.sockets[0].getsockname())
            data = bytes(range(256)) * 4096
            writer.write(data)
            await writer.drain()
            writer.write_eof()
            self.assertEqual(await asyncio.wait_for(reader.read(), 5), data[::-1])
            writer.close()
            await writer.wait_closed()
        finally:
            proxy.close()
            server_proxy.close()
            upstream.close()
            await proxy.wait_closed()
            await server_proxy.wait_closed()
            await upstream.wait_closed()

    async def test_tcp_rejects_other_source(self):
        contacted = asyncio.Event()

        async def unexpected(reader, writer):
            contacted.set()
            writer.close()

        upstream = await asyncio.start_server(unexpected, "127.0.0.1", 0)
        remote = upstream.sockets[0].getsockname()
        proxy = await asyncio.start_server(
            lambda r, w: bridge.relay_tcp(r, w, remote, {"192.0.2.1"}), "127.0.0.1", 0,
        )
        try:
            reader, writer = await asyncio.open_connection(*proxy.sockets[0].getsockname())
            self.assertEqual(await asyncio.wait_for(reader.read(), 2), b"")
            self.assertFalse(contacted.is_set())
            writer.close()
            await writer.wait_closed()
        finally:
            proxy.close()
            upstream.close()
            await proxy.wait_closed()
            await upstream.wait_closed()

    async def test_two_hop_udp_separates_clients_and_expires_flows(self):
        class Echo(asyncio.DatagramProtocol):
            def connection_made(self, transport):
                self.transport = transport

            def datagram_received(self, data, client):
                self.transport.sendto(data[::-1], client)

        loop = asyncio.get_running_loop()
        upstream, _ = await loop.create_datagram_endpoint(Echo, local_addr=("127.0.0.1", 0))
        remote = upstream.get_extra_info("sockname")
        server_proxy, server_relay = await loop.create_datagram_endpoint(
            lambda: bridge.UdpRelay(remote, {"127.0.0.1"}), local_addr=("127.0.0.1", 0),
        )
        server_address = server_proxy.get_extra_info("sockname")
        proxy, relay = await loop.create_datagram_endpoint(
            lambda: bridge.UdpRelay(server_address, {"127.0.0.1"}), local_addr=("127.0.0.1", 0),
        )
        clients = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
        try:
            for client in clients:
                client.setblocking(False)
            for index, client in enumerate(clients):
                await loop.sock_sendto(client, f"client {index}".encode(), proxy.get_extra_info("sockname"))
            for index, client in enumerate(clients):
                data, _ = await asyncio.wait_for(loop.sock_recvfrom(client, 65535), 2)
                self.assertEqual(data, f"client {index}".encode()[::-1])
            self.assertEqual(len(relay.flows), 2)
            self.assertEqual(len(server_relay.flows), 2)
            server_relay.datagram_received(b"rejected", ("192.0.2.1", 12345))
            self.assertEqual(len(server_relay.flows), 2)
            for flow in relay.flows.values():
                flow[1] = 0
            relay.expire()
            self.assertFalse(relay.flows)
            relay.datagram_received(b"rejected", ("192.0.2.1", 12345))
            self.assertFalse(relay.flows)
        finally:
            for client in clients:
                client.close()
            proxy.close()
            server_proxy.close()
            upstream.close()
            relay.connection_lost(None)
            server_relay.connection_lost(None)

    async def test_server_requires_explicit_listen_address_and_clients(self):
        for bind, allow in ((None, ["10.0.0.2"]), ("10.0.0.1", []), ("0.0.0.0", ["10.0.0.2"])):
            args = SimpleNamespace(command="relay", bind=bind, allow=allow, via=None, timeout=1800)
            with self.subTest(bind=bind, allow=allow), self.assertRaises(ValueError):
                await bridge.connect(args)

    async def test_client_via_uses_server_and_advertises_own_lan_ip(self):
        record = dict(instance="UUID", host="Test.local.", port=49152, txt=["identifier=UUID", "authTag=test"])
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.json"
            config.write_text(bridge.json.dumps(dict(iphone="10.20.0.5", record=record)))
            args = SimpleNamespace(command="connect", config=config, iphone=None, interface="lo0", ports="55000", via="10.30.0.1", bind=None, allow=[], timeout=1800)
            writer = Mock(wait_closed=AsyncMock())
            server = Mock(wait_closed=AsyncMock())
            transport = Mock()
            registration = Mock(returncode=None, stdout=Mock(readline=AsyncMock(return_value=b"Name now registered and active\n")), wait=AsyncMock())
            loop = asyncio.get_running_loop()
            with (
                patch.object(bridge.subprocess, "check_output", return_value="192.168.1.2"),
                patch.object(bridge.socket, "if_nametoindex", return_value=1),
                patch.object(bridge.resource, "setrlimit"),
                patch.object(loop, "add_signal_handler", side_effect=lambda sig, callback: callback()),
                patch.object(bridge.asyncio, "open_connection", new=AsyncMock(return_value=(None, writer))) as opened,
                patch.object(bridge.asyncio, "start_server", new=AsyncMock(return_value=server)),
                patch.object(loop, "create_datagram_endpoint", new=AsyncMock(return_value=(transport, Mock()))),
                patch.object(bridge.asyncio, "create_subprocess_exec", new=AsyncMock(return_value=registration)) as advertised,
            ):
                await bridge.connect(args)
            opened.assert_awaited_once_with("10.30.0.1", 49152)
            self.assertEqual(advertised.call_args.args[8:10], ("yard-ios-192-168-1-2.local.", "192.168.1.2"))

    async def test_server_uses_no_apple_tools_and_stops_with_active_client(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("127.0.0.1", 0))
            pairing_port = reservation.getsockname()[1]
        record = dict(instance="UUID", host="Test.local.", port=pairing_port, txt=["identifier=UUID", "authTag=test"])
        loop = asyncio.get_running_loop()
        ready = asyncio.Event()
        accepted = asyncio.Event()
        closed = asyncio.Event()
        callbacks = {}
        real_start_server = asyncio.start_server

        async def start_server(callback, host, port):
            server = await real_start_server(callback, host, 0)
            addresses.append(server.sockets[0].getsockname())
            ready.set()
            return server

        async def hold_client(reader, writer, remote, allowed_ips):
            self.assertEqual(remote, ("10.20.0.5", pairing_port))
            self.assertEqual(allowed_ips, {"127.0.0.1", "10.0.0.2"})
            accepted.set()
            try:
                await asyncio.Event().wait()
            finally:
                writer.close()
                await writer.wait_closed()
                closed.set()

        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.json"
            config.write_text(bridge.json.dumps(dict(iphone="10.20.0.5", record=record)))
            args = SimpleNamespace(command="relay", config=config, iphone=None, ports=str(pairing_port), via=None, bind="127.0.0.1", allow=["127.0.0.1", "10.0.0.2"], timeout=1800)
            addresses = []
            writer = Mock(wait_closed=AsyncMock())
            real_open_connection = asyncio.open_connection
            with (
                patch.object(bridge.subprocess, "check_output", side_effect=AssertionError("Apple tool called")),
                patch.object(bridge.resource, "setrlimit"),
                patch.object(loop, "add_signal_handler", side_effect=lambda sig, callback: callbacks.setdefault(sig, callback)),
                patch.object(bridge.asyncio, "open_connection", new=AsyncMock(return_value=(None, writer))),
                patch.object(bridge.asyncio, "start_server", side_effect=start_server),
                patch.object(bridge.asyncio, "create_subprocess_exec", side_effect=AssertionError("Bonjour started on server")),
                patch.object(bridge, "relay_tcp", side_effect=hold_client),
            ):
                task = asyncio.create_task(bridge.connect(args))
                client = None
                try:
                    await asyncio.wait_for(ready.wait(), 2)
                    reader, client = await real_open_connection(*addresses[0])
                    await asyncio.wait_for(accepted.wait(), 2)
                    callbacks[bridge.signal.SIGTERM]()
                    await asyncio.wait_for(task, 2)
                    self.assertTrue(closed.is_set())
                    self.assertEqual(await asyncio.wait_for(reader.read(), 2), b"")
                finally:
                    if not task.done():
                        task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
                    if client:
                        client.close()
                        await client.wait_closed()


if __name__ == "__main__":
    unittest.main()
