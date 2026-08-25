# Build context is the repo root.
FROM rust:1-bookworm AS builder
WORKDIR /build

# `aws-lc-sys` needs a C toolchain. We avoid it where we can — jsonwebtoken uses
# the pure-Rust backend — but rustls may still pull it in transitively, and
# cross-arch builds fail cryptically without these.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang pkg-config \
 && rm -rf /var/lib/apt/lists/*

# Manifests first, with stub sources, so the dependency build caches
# independently of application changes. Same trick as stf-ios-provider's
# Dockerfile — dependency compilation dominates this build.
COPY Cargo.toml Cargo.lock ./
COPY packages/provider/crates/yard-protocol/Cargo.toml packages/provider/crates/yard-protocol/
COPY packages/provider/crates/provider-core/Cargo.toml packages/provider/crates/provider-core/
COPY packages/provider/crates/adb-bridge/Cargo.toml    packages/provider/crates/adb-bridge/
COPY packages/provider/crates/backend-mock/Cargo.toml  packages/provider/crates/backend-mock/
COPY packages/provider/crates/backend-ios/Cargo.toml   packages/provider/crates/backend-ios/
COPY packages/provider/crates/backend-android/Cargo.toml packages/provider/crates/backend-android/
COPY packages/provider/crates/yard-provider/Cargo.toml packages/provider/crates/yard-provider/
# The scrcpy server is embedded with include_bytes!, so it has to be present
# for the stub build too — a missing file here fails compilation, not linking.
COPY packages/provider/vendor ./packages/provider/vendor
RUN mkdir -p packages/provider/crates/yard-protocol/src \
             packages/provider/crates/provider-core/src \
             packages/provider/crates/adb-bridge/src \
             packages/provider/crates/backend-mock/src \
             packages/provider/crates/backend-ios/src \
             packages/provider/crates/backend-android/src \
             packages/provider/crates/yard-provider/src \
 && echo "" > packages/provider/crates/yard-protocol/src/lib.rs \
 && echo "" > packages/provider/crates/provider-core/src/lib.rs \
 && echo "" > packages/provider/crates/adb-bridge/src/lib.rs \
 && echo "" > packages/provider/crates/backend-mock/src/lib.rs \
 && echo "" > packages/provider/crates/backend-ios/src/lib.rs \
 && echo "" > packages/provider/crates/backend-android/src/lib.rs \
 && echo "fn main() {}" > packages/provider/crates/yard-provider/src/main.rs \
 && cargo build --release -p yard-provider \
 && rm -rf packages/provider/crates/*/src

COPY packages/provider ./packages/provider
# Touch so cargo does not trust the stub build's timestamps.
RUN find packages/provider -name '*.rs' -exec touch {} + \
 && cargo build --release -p yard-provider

# debian-slim rather than distroless/cc: the Android backend needs an adb
# server, which owns the USB transport and cannot be spoken to over TCP without
# one existing somewhere.
FROM debian:bookworm-slim AS runtime

# ca-certificates is required, not optional: the control plane and the JWKS
# fetch both use the OS trust store (see reqwest's rustls-native-certs).
#
# adb and usbmuxd are the two backends' host dependencies, and the container
# runs both itself rather than borrowing the host's: a daemon whose lifecycle is
# the host's is one the container cannot restart, and usbmuxd's in particular
# ends at the last unplugged device. The scrcpy server needs no install — it is
# embedded in the binary and pushed to the phone at session start.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates adb usbmuxd \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /var/lib/yard/scratch /var/lib/lockdown

COPY --from=builder /build/target/release/yard-provider /usr/local/bin/yard-provider
COPY packages/provider/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Runs as root, unlike the coordinator: on Linux the container owns the USB bus,
# and the /dev/bus/usb
# nodes are root-owned — `privileged` grants the device access, not the file
# permission. The adb server needs to write to them, so it has to be root. The
# process still has nothing on disk worth reaching: the only writable path is
# the scratch tmpfs, which nothing outlives an install in.
EXPOSE 7100
ENV PROVIDER_CONFIG=/etc/yard/provider.yaml

# Self-probe: the session plane is what browsers depend on, so that is what
# health means here.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/yard-provider", "--config", "/etc/yard/provider.yaml", "--check"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
