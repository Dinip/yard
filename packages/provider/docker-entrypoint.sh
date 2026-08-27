#!/bin/sh
# The provider speaks adb's and usbmuxd's protocols but never owns a USB
# transport itself, so on a Linux host — where the container owns /dev/bus/usb —
# something has to start the two daemons that do. That is this script's whole
# job.
#
# Both are best-effort: the macOS overlay points at the host's daemons instead,
# and there a local one would be a second owner of nothing. A failure here is
# logged and the provider starts anyway; each backend's own retry loop reports
# the refusal if it mattered.
#
# PROVIDER_START_ADB and PROVIDER_START_USBMUXD are `auto` (start it if this
# container can see a USB bus), `yes`, or `no`. Set one of them to `no` to run
# one provider per platform on a single host — see docs/PROVIDER.md.
set -e

# adb generates a keypair here on first start and the phone's "Allow USB
# debugging" grant is bound to its fingerprint, so this directory is mounted
# from a volume — regenerating it means walking to the device and tapping the
# dialog again after every recreate.
export HOME="${HOME:-/root}"
mkdir -p "$HOME/.android"

# `auto` means "is there a bus in here to own": on macOS there is not, because
# Docker cannot pass USB through, and the daemons live on the host.
has_usb_bus() {
	[ -d /dev/bus/usb ]
}

wants() {
	case "$1" in
	yes | true | 1) return 0 ;;
	no | false | 0) return 1 ;;
	*) has_usb_bus ;;
	esac
}

if wants "${PROVIDER_START_ADB:-auto}" && command -v adb >/dev/null 2>&1; then
	# `start-server` daemonises and returns, unlike `nodaemon server`, which is
	# what lets this stay a wrapper rather than a supervisor.
	if adb start-server >/dev/null 2>&1; then
		echo "adb server listening on 127.0.0.1:5037 (keys in $HOME/.android)"
	else
		echo "warning: could not start the adb server; Android devices will be unreachable" >&2
	fi
fi

if wants "${PROVIDER_START_USBMUXD:-auto}" && command -v usbmuxd >/dev/null 2>&1; then
	# Pair records and the host identity they are issued against live here, and
	# it is a volume for the same reason the adb key is: losing it means walking
	# to every iPhone and tapping Trust This Computer again.
	mkdir -p /var/lib/lockdown

	# Supervised, unlike adb, and for the reason the container runs its own at
	# all: usbmuxd exits when the last device is unplugged, and the udev rule
	# that would bring it back is the host's.
	#
	# The watchdog is what replaces udev in here, and a rescan is not enough.
	# libusb learns about a plug or an unplug from udevd's netlink broadcast,
	# which reaches only udevd's own network namespace, so in a container its
	# device list is frozen at the moment usbmuxd started: a device that comes
	# back on a new bus address is invisible, and the one it replaced is still
	# in the list, failing to open forever (LIBUSB_ERROR_IO, errno 19). Only a
	# fresh libusb — a fresh process — sees the bus as it is now.
	#
	# sysfs, unlike libusb's cache, is the host's and always current, so the
	# watchdog polls it for Apple devices and restarts usbmuxd when the set
	# changes. That costs every *other* iPhone its usbmuxd connection, hence
	# the settle tick: a device that re-enumerates passes through intermediate
	# states, and each of them would otherwise be its own restart.
	apple_devices() {
		for device in /sys/bus/usb/devices/*; do
			[ -r "$device/idVendor" ] || continue
			[ "$(cat "$device/idVendor")" = "05ac" ] || continue
			echo "$device $(cat "$device/devnum" 2>/dev/null)"
		done
	}

	(
		watch_seconds="${PROVIDER_USBMUXD_WATCH_SECONDS:-2}"
		while true; do
			usbmuxd -f &
			muxer=$!
			seen=$(apple_devices)
			pending=""
			while kill -0 "$muxer" 2>/dev/null; do
				sleep "$watch_seconds"
				now=$(apple_devices)
				if [ "$now" = "$seen" ]; then
					pending=""
					continue
				fi
				if [ "$now" != "$pending" ]; then
					pending=$now
					continue
				fi
				echo "usb devices changed; restarting usbmuxd to pick them up" >&2
				kill "$muxer" 2>/dev/null || true
				break
			done
			wait "$muxer" || true
			sleep 1
		done
	) &

	echo "usbmuxd listening on /var/run/usbmuxd (pair records in /var/lib/lockdown)"
fi

exec /usr/local/bin/yard-provider "$@"
