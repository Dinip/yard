# Build context is the repo root.
FROM debian:bookworm-slim AS ffmpeg-builder
WORKDIR /build

# Keep Debian's patched source, without the distro binary's video/graphics deps.
RUN sed -i 's/Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config nasm libopus-dev \
 && apt-get source ffmpeg \
 && cd ffmpeg-* \
 && ./configure \
      --disable-autodetect --disable-everything --disable-network \
      --disable-doc --disable-debug --disable-ffplay --disable-ffprobe \
      --disable-avdevice --disable-swscale --disable-postproc \
      --enable-decoder=aac --enable-demuxer=matroska \
      --enable-libopus --enable-encoder=libopus --enable-muxer=opus \
      --enable-protocol=pipe --enable-filter=aresample,aformat,anull \
 && make -j"$(nproc)" ffmpeg \
 && install -m 755 ffmpeg /usr/local/bin/ffmpeg

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
# FFmpeg's only external codec dependency is libopus.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates adb libopus0 \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /var/lib/yard/scratch

COPY --from=ffmpeg-builder /usr/local/bin/ffmpeg /usr/local/bin/ffmpeg
COPY --from=builder /build/target/release/yard-provider /usr/local/bin/yard-provider
COPY packages/provider/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# adb needs root access to the USB nodes.
EXPOSE 7100
ENV PROVIDER_CONFIG=/etc/yard/provider.yaml

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/yard-provider", "--config", "/etc/yard/provider.yaml", "--check"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
