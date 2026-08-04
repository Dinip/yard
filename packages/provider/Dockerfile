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
COPY packages/provider/crates/farm-provider/Cargo.toml packages/provider/crates/farm-provider/
RUN mkdir -p packages/provider/crates/farm-protocol/src \
             packages/provider/crates/provider-core/src \
             packages/provider/crates/backend-mock/src \
             packages/provider/crates/farm-provider/src \
 && echo "" > packages/provider/crates/farm-protocol/src/lib.rs \
 && echo "" > packages/provider/crates/provider-core/src/lib.rs \
 && echo "" > packages/provider/crates/backend-mock/src/lib.rs \
 && echo "fn main() {}" > packages/provider/crates/farm-provider/src/main.rs \
 && cargo build --release -p farm-provider \
 && rm -rf packages/provider/crates/*/src

COPY packages/provider ./packages/provider
# Touch so cargo does not trust the stub build's timestamps.
RUN find packages/provider -name '*.rs' -exec touch {} + \
 && cargo build --release -p farm-provider

# debian-slim rather than distroless/cc: the Android backend needs the `adb`
# binary and the vendored scrcpy-server jar (phase 4).
FROM debian:bookworm-slim AS runtime

# ca-certificates is required, not optional: the control plane and the JWKS
# fetch both use the OS trust store (see reqwest's rustls-native-certs).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
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
