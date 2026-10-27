#!/usr/bin/env python3
"""Generate a target's Info.plist using the App Group selected in Xcode."""

from pathlib import Path
import plistlib
import sys


def generate_info(source, entitlements, output):
    groups = plistlib.loads(entitlements.read_bytes()).get(
        "com.apple.security.application-groups", []
    )
    if (not isinstance(groups, list) or len(groups) != 1
            or not isinstance(groups[0], str) or not groups[0].startswith("group.")):
        raise ValueError(f"Select exactly one App Group in Signing & Capabilities for {entitlements}.")
    info = plistlib.loads(source.read_bytes())
    info["YardAppGroup"] = groups[0]
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(plistlib.dumps(info))


if __name__ == "__main__":
    try:
        generate_info(*(Path(argument) for argument in sys.argv[1:]))
    except (OSError, ValueError, plistlib.InvalidFileException) as error:
        sys.exit(f"error: {error}")
