#!/bin/sh
# Start adb when the provider owns the USB bus.
set -e

export HOME="${HOME:-/root}"
mkdir -p "$HOME/.android"

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
	if adb start-server >/dev/null 2>&1; then
		echo "adb server listening on 127.0.0.1:5037 (keys in $HOME/.android)"
	else
		echo "warning: could not start the adb server; Android devices will be unreachable" >&2
	fi
fi

exec /usr/local/bin/yard-provider "$@"
