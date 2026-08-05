# Build context is the repo root.
FROM oven/bun:1 AS builder
WORKDIR /app

COPY package.json bun.lock bunfig.toml ./
COPY packages/protocol/package.json ./packages/protocol/
COPY packages/db/package.json ./packages/db/
COPY packages/coordinator/package.json ./packages/coordinator/
COPY packages/web/package.json ./packages/web/
RUN bun install --frozen-lockfile

COPY tsconfig.base.json ./
# The SPA imports the coordinator's router *type* only; the source must be
# present for tsc/vite to resolve it, but none of it reaches the bundle.
COPY packages/protocol ./packages/protocol
COPY packages/db ./packages/db
COPY packages/coordinator ./packages/coordinator
COPY packages/web ./packages/web

WORKDIR /app/packages/web
RUN bun run build

FROM caddy:2-alpine AS runtime
COPY --from=builder /app/packages/web/dist /srv
COPY packages/web/Caddyfile /etc/caddy/Caddyfile
EXPOSE 80
