/**
 * Expose the host's usbmuxd socket over TCP, for a containerised provider.
 *
 * `bun packages/provider/scripts/usbmuxd-bridge.ts`
 *
 * ## Why this exists
 *
 * On Linux a provider container reaches iOS devices by bind-mounting
 * `/var/run/usbmuxd`, and this script has no reason to run. On **macOS**
 * neither half of that works: Docker Desktop cannot pass USB through to its
 * Linux VM, and it cannot bind-mount a macOS unix socket into a container
 * either. usbmuxd only exists on the host, so the only way in is TCP.
 *
 * The provider then gets `USBMUXD_SOCKET_ADDRESS=host.docker.internal:27015`.
 * `stf-ios-provider` used a socat container for the same job; this is the same
 * shim with no extra dependency, since bun is already here.
 *
 * ## What it is not
 *
 * Not a production component. It binds loopback only, because anything that can
 * reach it can drive every iOS device on this machine — pairing records
 * included — with no authentication whatsoever.
 */

import { existsSync } from "node:fs";
import { connect } from "node:net";

const USBMUXD_SOCKET = "/var/run/usbmuxd";
const PORT = Number(process.env.USBMUXD_BRIDGE_PORT ?? 27015);

if (!existsSync(USBMUXD_SOCKET)) {
  console.error(
    `[usbmuxd-bridge] ${USBMUXD_SOCKET} does not exist. On macOS it appears once a device has been plugged in at least once; on Linux the container should mount it directly instead of using this bridge.`,
  );
  process.exit(1);
}

const server = Bun.listen<undefined>({
  hostname: "127.0.0.1",
  port: PORT,
  socket: {
    open(socket) {
      // One unix connection per TCP connection: usbmuxd multiplexes at the
      // protocol level, not the socket level, and sharing one would interleave
      // two clients' replies.
      const upstream = connect(USBMUXD_SOCKET);
      socket.data = undefined;

      upstream.on("data", (chunk) => socket.write(chunk));
      upstream.on("error", (error) => {
        console.warn("[usbmuxd-bridge] upstream error:", error.message);
        socket.end();
      });
      upstream.on("close", () => socket.end());

      // Stash it where the other handlers can reach it.
      (socket as unknown as { upstream: typeof upstream }).upstream = upstream;
    },
    data(socket, chunk) {
      const { upstream } = socket as unknown as { upstream: ReturnType<typeof connect> };
      upstream.write(chunk);
    },
    close(socket) {
      const { upstream } = socket as unknown as { upstream: ReturnType<typeof connect> };
      upstream?.end();
    },
    error(socket) {
      const { upstream } = socket as unknown as { upstream: ReturnType<typeof connect> };
      upstream?.end();
    },
  },
});

console.log(
  `[usbmuxd-bridge] ${USBMUXD_SOCKET} → 127.0.0.1:${server.port}\n` +
    `[usbmuxd-bridge] point the provider at it with USBMUXD_SOCKET_ADDRESS=host.docker.internal:${server.port}`,
);
