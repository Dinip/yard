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
COPY packages/provider/crates/farm-protocol/Cargo.toml packages/provider/crates/farm-protocol/
COPY packages/provider/crates/provider-core/Cargo.toml packages/provider/crates/provider-core/
COPY packages/provider/crates/backend-mock/Cargo.toml  packages/provider/crates/backend-mock/
COPY packages/provider/crates/backend-ios/Cargo.toml   packages/provider/crates/backend-ios/
COPY packages/provider/crates/backend-android/Cargo.toml packages/provider/crates/backend-android/
COPY packages/provider/crates/farm-provider/Cargo.toml packages/provider/crates/farm-provider/
# The scrcpy server is embedded with include_bytes!, so it has to be present
# for the stub build too — a missing file here fails compilation, not linking.
COPY packages/provider/vendor ./packages/provider/vendor
RUN mkdir -p packages/provider/crates/farm-protocol/src \
             packages/provider/crates/provider-core/src \
             packages/provider/crates/backend-mock/src \
             packages/provider/crates/backend-ios/src \
             packages/provider/crates/backend-android/src \
             packages/provider/crates/farm-provider/src \
 && echo "" > packages/provider/crates/farm-protocol/src/lib.rs \
 && echo "" > packages/provider/crates/provider-core/src/lib.rs \
 && echo "" > packages/provider/crates/backend-mock/src/lib.rs \
 && echo "" > packages/provider/crates/backend-ios/src/lib.rs \
 && echo "" > packages/provider/crates/backend-android/src/lib.rs \
 && echo "fn main() {}" > packages/provider/crates/farm-provider/src/main.rs \
 && cargo build --release -p farm-provider \
 && rm -rf packages/provider/crates/*/src

COPY packages/provider ./packages/provider
# Touch so cargo does not trust the stub build's timestamps.
RUN find packages/provider -name '*.rs' -exec touch {} + \
 && cargo build --release -p farm-provider

# debian-slim rather than distroless/cc: the Android backend needs an adb
# server, which owns the USB transport and cannot be spoken to over TCP without
# one existing somewhere.
FROM debian:bookworm-slim AS runtime

# ca-certificates is required, not optional: the control plane and the JWKS
# fetch both use the OS trust store (see reqwest's rustls-native-certs).
#
# adb is the Android backend's one host dependency. The scrcpy server needs no
# install — it is embedded in the binary and pushed to the phone at session
# start — and iOS needs only usbmuxd, which is the host's.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates adb \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home farm \
 && mkdir -p /var/lib/farm/scratch \
 && chown -R farm:farm /var/lib/farm

COPY --from=builder /build/target/release/farm-provider /usr/local/bin/farm-provider

USER farm
EXPOSE 7100
ENV FARM_CONFIG=/etc/farm/provider.yaml

# Self-probe: the session plane is what browsers depend on, so that is what
# health means here.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/farm-provider", "--config", "/etc/farm/provider.yaml", "--check"]

ENTRYPOINT ["/usr/local/bin/farm-provider"]
