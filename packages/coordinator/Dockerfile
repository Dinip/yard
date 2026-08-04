# Build context is the repo root.
FROM oven/bun:1 AS builder
WORKDIR /app

# Manifests first so the install layer caches independently of source changes.
COPY package.json bun.lock bunfig.toml ./
COPY packages/db/package.json ./packages/db/
COPY packages/coordinator/package.json ./packages/coordinator/
COPY packages/web/package.json ./packages/web/
RUN bun install --frozen-lockfile

COPY tsconfig.base.json ./
COPY packages/db ./packages/db
COPY packages/coordinator ./packages/coordinator

# Built separately rather than as two entrypoints of one invocation: bun mirrors
# the entrypoints' shared directory prefix into the outdir, which for these two
# is `packages/`, and the resulting nesting is not worth reasoning about.
RUN mkdir -p /app/out \
 && bun build packages/coordinator/src/index.ts \
      --target=bun --sourcemap=linked --outdir /app/out \
 && bun build packages/db/src/migrate.ts \
      --target=bun --sourcemap=linked --outdir /app/out

FROM oven/bun:1-slim AS runtime
WORKDIR /app
ENV NODE_ENV=production

COPY --from=builder /app/out ./
# Migrations are applied by the entrypoint, so the SQL must ship with the image.
COPY --from=builder /app/packages/db/drizzle ./drizzle
COPY packages/coordinator/docker-entrypoint.sh /usr/local/bin/entrypoint
RUN chmod +x /usr/local/bin/entrypoint

USER bun
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/entrypoint"]
CMD ["bun", "/app/index.js"]
