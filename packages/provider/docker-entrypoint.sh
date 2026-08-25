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
	# The SIGUSR2 nudge is what replaces udev in here. usbmuxd polls the bus
	# every second only until it manages to register for libusb hotplug events —
	# which succeeds in a container and then delivers nothing, because libusb
	# watches udevd's netlink broadcast and that reaches only udevd's own
	# network namespace. So it would sit there, poll disabled, deaf to every
	# device plugged in after start. -z is what makes SIGUSR2 mean "rescan the
	# bus"; discovery is a device-list walk, the same one the poll it replaces
	# was doing. -n is not the answer: it turns the poll off too.
	(
		while true; do
			usbmuxd -f -z &
			muxer=$!
			while kill -0 "$muxer" 2>/dev/null; do
				sleep "${PROVIDER_USBMUXD_RESCAN_SECONDS:-2}"
				kill -USR2 "$muxer" 2>/dev/null || true
			done
			wait "$muxer" || true
			echo "warning: usbmuxd exited; restarting" >&2
			sleep 1
		done
	) &

	echo "usbmuxd listening on /var/run/usbmuxd (pair records in /var/lib/lockdown)"
fi

exec /usr/local/bin/yard-provider "$@"
