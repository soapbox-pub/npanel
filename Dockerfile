# npanel in a container: one binary, and the dashboard it serves.
#
# The result speaks plain HTTP on port 3000 and expects a reverse proxy in front
# of it — see compose.yaml, which pairs it with Caddy.

FROM rust:1-bookworm AS server

WORKDIR /src

# Dependencies first, so a source change doesn't rebuild the world.
COPY Cargo.toml Cargo.lock ./
COPY vendor ./vendor
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src

COPY assets ./assets
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# The dashboard is built separately because it needs its own toolchain (vite,
# react and the rest), none of which belong in the runtime image.
FROM oven/bun:1.3-alpine AS dashboard

WORKDIR /app/dashboard

COPY dashboard/package.json dashboard/bun.lock ./
RUN bun install --frozen-lockfile

COPY dashboard ./
RUN bun run build

FROM debian:bookworm-slim

# wget is for the health check. Everything npanel itself needs — TLS roots, the
# time zone database, the device database — is compiled into the binary.
RUN apt-get update && apt-get install -y --no-install-recommends wget && rm -rf /var/lib/apt/lists/*

# uid and gid 1000, which the Tor image's user shares a group with so that npanel
# can read Tor's control cookie.
RUN groupadd --gid 1000 npanel && useradd --uid 1000 --gid npanel --home /app --shell /usr/sbin/nologin npanel

WORKDIR /app

COPY --from=server /src/target/release/npanel /usr/local/bin/npanel
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

# Analytics events, received by a thread of their own. The proxy routes one
# path on every site here.
ENV ANALYTICS_PORT=3002

# The database and the blob store. Mount a volume here or the configuration
# lives only as long as the container does; compose.yaml declares one.
# Pre-created so a fresh volume inherits an owner the server can write as.
ENV DATA_DIR=/data
RUN mkdir -p /data && chown npanel:npanel /data

USER npanel

EXPOSE 3000 3001 3002

# Both of the listeners that make a gateway, since either one being down is a
# broken one — analytics is left out, because a gateway that can't count visits
# still serves them. /api/instance is unauthenticated but served only on the
# dashboard hostname, hence the Host header, and answering it means the database
# opened; the second call proves the proxy can still get certificates
# authorized. The dashboard hostname is never metered, so this spends no part of
# the issuance budget.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O /dev/null --header "host: ${DASHBOARD_HOST}" "http://127.0.0.1:${PORT}/api/instance" && \
      wget -q -O /dev/null "http://127.0.0.1:${TLS_CHECK_PORT}/tls-check?domain=${DASHBOARD_HOST}"

CMD ["npanel"]
