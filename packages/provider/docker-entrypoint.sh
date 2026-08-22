#!/bin/sh
# The provider speaks adb's TCP protocol but never spawns the binary, so on a
# Linux host — where the container owns /dev/bus/usb — something has to start
# the server that owns the USB transport. That is this script's whole job.
#
# It is deliberately best-effort: the macOS overlay points `adb_server` at the
# host's server instead, and there a local one would be a second owner of
# nothing. A failure here is logged and the provider starts anyway; the Android
# backend's own retry loop reports the connection refusal if it mattered.
set -e

# adb generates a keypair here on first start and the phone's "Allow USB
# debugging" grant is bound to its fingerprint, so this directory is mounted
# from a volume — regenerating it means walking to the device and tapping the
# dialog again after every recreate.
export HOME="${HOME:-/root}"
mkdir -p "$HOME/.android"

if command -v adb >/dev/null 2>&1; then
	# `start-server` daemonises and returns, unlike `nodaemon server`, which is
	# what lets this stay a wrapper rather than a supervisor.
	if adb start-server >/dev/null 2>&1; then
		echo "adb server listening on 127.0.0.1:5037 (keys in $HOME/.android)"
	else
		echo "warning: could not start the adb server; Android devices will be unreachable" >&2
	fi
fi

exec /usr/local/bin/yard-provider "$@"
