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
# The context has no .git, so the sha the UI shows can only come from outside.
ARG GIT_SHA
ENV GIT_SHA=$GIT_SHA
RUN bun run build

FROM caddy:2-alpine AS runtime
COPY --from=builder /app/packages/web/dist /srv
COPY packages/web/Caddyfile /etc/caddy/Caddyfile
EXPOSE 80

# Health is "the bundle is being served", not "the process is up": a Caddy that
# started with an empty /srv answers every request with a 404 and looks fine to
# anything that only checks the port.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD ["wget", "--spider", "-q", "http://127.0.0.1/index.html"]
