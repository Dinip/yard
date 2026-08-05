# Build context is the repo root.
FROM oven/bun:1 AS builder
WORKDIR /app

# Manifests first so the install layer caches independently of source changes.
COPY package.json bun.lock bunfig.toml ./
COPY packages/protocol/package.json ./packages/protocol/
COPY packages/db/package.json ./packages/db/
COPY packages/coordinator/package.json ./packages/coordinator/
COPY packages/web/package.json ./packages/web/
RUN bun install --frozen-lockfile

COPY tsconfig.base.json ./
# `packages/protocol` is not optional here even though nothing in this image is
# named after it. Leaving it out does not fail the build — bun resolves the
# workspace import to a broken symlink and emits a bundle without it — it fails
# at *runtime*, silently: JWKS 404s, the control plane cannot parse a single
# provider message, and session tokens carry no audience. The container starts
# and looks healthy.
COPY packages/protocol ./packages/protocol
COPY packages/db ./packages/db
COPY packages/coordinator ./packages/coordinator

# Built separately rather than as two entrypoints of one invocation: bun mirrors
# the entrypoints' shared directory prefix into the outdir, which for these two
# is `packages/`, and the resulting nesting is not worth reasoning about.
RUN mkdir -p /app/out \
 && bun build packages/coordinator/src/index.ts \
      --target=bun --sourcemap=linked --outdir /app/out \
 && bun build packages/db/src/migrate.ts \
      --target=bun --sourcemap=linked --outdir /app/out \
 && bun build packages/coordinator/src/cli/grant-admin.ts \
      --target=bun --sourcemap=linked --outdir /app/out

FROM oven/bun:1-slim AS runtime
WORKDIR /app
ENV NODE_ENV=production

COPY --from=builder /app/out ./
# grant-admin ships too: a fresh deployment has no admin, and promoting the
# first one must not require a psql session against the production database.
#   docker compose exec coordinator bun /app/grant-admin.js you@example.com
#
# Migrations are applied by the entrypoint, so the SQL must ship with the image.
COPY --from=builder /app/packages/db/drizzle ./drizzle
COPY packages/coordinator/docker-entrypoint.sh /usr/local/bin/entrypoint
RUN chmod +x /usr/local/bin/entrypoint

USER bun
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/entrypoint"]
CMD ["bun", "/app/index.js"]
