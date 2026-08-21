# npanel in a container: Bun runs the server's TypeScript directly, so the only
# thing to build is the dashboard SPA.
#
# The result speaks plain HTTP on port 3000 and expects a reverse proxy in front
# of it — see compose.yaml, which pairs it with Caddy.

# Server dependencies only. Nothing in devDependencies (biome, tsc) runs in
# production.
FROM oven/bun:1.3-alpine AS deps

WORKDIR /app

COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production

# The dashboard is built separately because it needs its own dependency tree
# (vite, react, and the rest), none of which belong in the runtime image.
FROM oven/bun:1.3-alpine AS dashboard

WORKDIR /app

COPY dashboard/package.json dashboard/bun.lock ./dashboard/
RUN cd dashboard && bun install --frozen-lockfile

# The dashboard imports the address parser from the server's src/ through the
# @shared alias. That file resolves its own imports — nostr-tools — against the
# root node_modules, so the server's dependencies have to be here too.
COPY --from=deps /app/node_modules ./node_modules
COPY src ./src
COPY dashboard ./dashboard
RUN cd dashboard && bun run build

FROM oven/bun:1.3-alpine

WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY package.json bun.lock tsconfig.json ./
COPY src ./src
COPY scripts ./scripts
# The server serves this directory as the dashboard; without it the dashboard
# hostname answers 503.
COPY --from=dashboard /app/dashboard/dist ./dashboard/dist

# Loopback would make npanel unreachable from any other container, including the
# proxy. Safe only because nothing publishes this port: npanel speaks plain
# HTTP, so it must stay on an internal network.
ENV BIND_ADDRESS=0.0.0.0
ENV PORT=3000

# Certificate authorization for the proxy, on a port of its own so that it is
# not reachable through any hostname npanel serves.
ENV TLS_CHECK_PORT=3001

# The database and the blob store. Mount a volume here or the configuration
# lives only as long as the container does; compose.yaml declares one.
# Pre-created so a fresh volume inherits an owner the server can write as.
ENV DATA_DIR=/data
RUN mkdir -p /data && chown bun:bun /data

USER bun

EXPOSE 3000 3001

# Both listeners, since either one being down is a broken gateway: /api/instance
# is unauthenticated but served only on the dashboard hostname, hence the Host
# header, and answering it means the database opened; the second call proves the
# proxy can still get certificates authorized. The dashboard hostname is never
# metered, so this spends no part of the issuance budget.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O /dev/null --header "host: ${DASHBOARD_HOST}" "http://127.0.0.1:${PORT}/api/instance" && \
      wget -q -O /dev/null "http://127.0.0.1:${TLS_CHECK_PORT}/tls-check?domain=${DASHBOARD_HOST}"

CMD ["bun", "run", "src/server.ts"]
