# npanel in a container: one binary, and the dashboard it serves.
#
# The result reads /etc/npanel/npanel.toml, speaks plain HTTP, and expects a
# reverse proxy in front of it — see compose.yaml, which writes the config and
# pairs it with Caddy.

FROM rust:1-bookworm AS server

WORKDIR /src

# Dependencies first, so a source change doesn't rebuild the world. The image's
# rustup installs whichever Rust rust-toolchain.toml names.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY vendor ./vendor
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked && rm -rf src

COPY build.rs ./
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
# The build stamps the server's version into the dashboard.
COPY Cargo.toml /app/Cargo.toml
RUN bun run build

FROM debian:bookworm-slim

# wget is for compose's health check, and the fonts are what sites' link-preview
# scripts draw text in. Everything else npanel needs — TLS roots, the time zone
# database, the device database — is compiled into the binary.
RUN apt-get update && apt-get install -y --no-install-recommends wget fonts-dejavu-core fonts-noto-color-emoji \
  && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 npanel && useradd --uid 1000 --gid npanel --home /app --shell /usr/sbin/nologin npanel

WORKDIR /app

COPY --from=server /src/target/release/npanel /usr/local/bin/npanel
# The server serves this directory as the dashboard; without it the dashboard
# hostname answers 503.
COPY --from=dashboard /app/dashboard/dist ./dashboard/dist

# The database and the blob store: data_dir = "/data" in the config. Mount a
# volume here or the configuration lives only as long as the container does;
# compose.yaml declares one. Pre-created so a fresh volume inherits an owner the
# server can write as.
RUN mkdir -p /data && chown npanel:npanel /data

USER npanel

EXPOSE 3000 3001

# The configuration is mounted at /etc/npanel/npanel.toml, where npanel looks
# for it; compose.yaml writes one. The health check is compose's too, since
# what to ask depends on what the file says.
CMD ["npanel"]
