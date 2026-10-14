#!/bin/sh
set -eu

# Run with Java 17 and Android SDK build-tools 36.1.0; provider builds embed the committed DEX.
provider_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
sdk_dir=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
: "${sdk_dir:?Set ANDROID_HOME to the Android SDK directory}"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
mkdir "$work_dir/classes" "$work_dir/dex"
javac --release 8 -d "$work_dir/classes" "$provider_dir/vendor/audio-volume/AudioVolume.java"
"$sdk_dir/build-tools/36.1.0/d8" --min-api 30 --output "$work_dir/dex" \
  "$work_dir/classes/com/yard/AudioVolume.class"
cp "$work_dir/dex/classes.dex" "$provider_dir/vendor/audio-volume.dex"
