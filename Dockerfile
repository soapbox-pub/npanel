# npanel in a container: the binary a release publishes, built the same way by
# the same script (contrib/release/build.sh), static and with the dashboard
# compiled in.
#
# The result reads /etc/npanel/npanel.toml, speaks plain HTTP, and expects a
# reverse proxy in front of it — see compose.yaml, which writes the config and
# pairs it with Caddy.

# The dashboard is built separately because it needs its own toolchain (vite,
# react and the rest), none of which belong in the runtime image. On the
# builder's own platform: its output is the same for every one.
FROM --platform=$BUILDPLATFORM oven/bun:1.3-alpine AS dashboard

WORKDIR /app/dashboard

COPY dashboard/package.json dashboard/bun.lock ./
RUN bun install --frozen-lockfile

COPY dashboard ./
# The build stamps the server's version into the dashboard.
COPY Cargo.toml /app/Cargo.toml
RUN bun run build

# On the builder's own platform too, cross-compiling with zig as a release
# does, so an arm64 image is built at native speed on an x86_64 machine.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS server

WORKDIR /src

# The image's rustup installs whichever Rust rust-toolchain.toml names, and its
# musl targets; then the pinned tools. Both are layers of their own, so a
# source change installs neither again.
COPY rust-toolchain.toml ./
RUN rustup toolchain install
COPY contrib/release ./contrib/release
RUN contrib/release/tools.sh /opt/tools > /opt/tools.path

COPY Cargo.toml Cargo.lock build.rs ./
COPY vendor ./vendor
COPY assets ./assets
COPY src ./src
COPY --from=dashboard /app/dashboard/dist ./dashboard/dist

# The registry and the target directory are caches of the builder's, so a
# source change rebuilds npanel and not its dependencies.
ARG TARGETARCH
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "npanel's image is built for amd64 or arm64, not $TARGETARCH." >&2; exit 1 ;; \
    esac; \
    PATH="$(cat /opt/tools.path):$PATH" contrib/release/build.sh /out "$arch"; \
    mv /out/npanel-*-linux-"$arch" /npanel

FROM debian:bookworm-slim

# wget is for compose's health check, and the fonts are what sites' link-preview
# scripts draw text in. Everything else npanel needs — the dashboard, TLS roots,
# the time zone database, the device database — is compiled into the binary.
RUN apt-get update && apt-get install -y --no-install-recommends wget fonts-dejavu-core fonts-noto-color-emoji \
  && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 npanel && useradd --uid 1000 --gid npanel --home /app --shell /usr/sbin/nologin npanel

COPY --from=server /npanel /usr/local/bin/npanel

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
