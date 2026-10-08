# npanel

A self-hostable gateway for
[static websites published on Nostr](https://github.com/nostr-protocol/nips/blob/master/5A.md)
(the nsite protocol), with a team dashboard.

Point your domains' A/AAAA records at one box, then map domains to nsites from
a web UI that works like a DNS editor. npanel downloads each site's files from
Blossom servers, stores them locally, and serves them — so a site stays up even
when the relays and Blossom servers it came from are down.

- **Permissioned.** Only people you add can log in. Staff see only the sites
  they've been granted; admins see everything.
- **Nostr-native login.** NIP-98 HTTP auth with a browser extension. No passwords.
- **Versioned.** Every site event is kept as a version you can inspect, pin, or
  roll back to, with a per-version deploy log that explains failures.
- **Auditable.** Every change to a site, user, or setting is recorded.
- **Tor built in.** Flip a switch and every site also gets its own `.onion`
  address, served by a Tor that runs inside npanel — no daemon to install.
- **Analytics built in.** Flip a switch on a site and npanel counts its visits
  the way Plausible does — no cookies, nothing identifying stored — and shows
  them in a Plausible-style dashboard.
- **Link previews for Nostr.** A site that shows Nostr posts at `/npub1…`,
  `/nevent1…` or `/naddr1…` can have crawlers given each post's own title,
  image and text, so links unfurl and search engines can index them.

## Performance

Measured on one machine, 64 concurrent connections, 10 seconds per row:

| Request                               | Throughput |
| ------------------------------------- | ---------: |
| `index.html` (2 KB, brotli)           | 70,219/s   |
| JS bundle (555 KB, 149 KB brotli)     | 28,856/s   |
| Image (284 KB, identity)              | 15,756/s   |
| Range request (1 KB)                  | 71,157/s   |
| SPA fallback                          | 69,224/s   |
| 404                                   | 75,010/s   |
| 304 revalidation                      | 93,974/s   |
| `GET /api/instance`                   | 92,065/s   |
| Dashboard `index.html`                | 81,309/s   |

Median latency is 0.6–3.7 ms. The two large files are limited by copying bytes
over loopback rather than by npanel. Anonymous memory after serving load and a
burst of analytics events is 119 MB; serving alone uses 35 MB. The analytics
user-agent parser — a Matomo port vendored in `vendor/device-detector` — parses
in well under a millisecond.

## Requirements

- Linux. Releases are static x86_64 and aarch64 binaries with the dashboard
  inside, which run on any distribution. Elsewhere, build it:
  [rustup](https://rustup.rs) for the server and [Bun](https://bun.sh) 1.3+ for
  the dashboard.
- A reverse proxy for TLS — [Caddy](https://caddyserver.com) is the easy path,
  and a sample `Caddyfile` is included.

Or just Docker, if you'd rather not install any of that.

## Quick start with Docker

`compose.yaml` runs npanel and a Caddy that gets a certificate for every
hostname your staff add:

```sh
cp .env.example .env
$EDITOR .env          # set DASHBOARD_HOST

docker compose up -d
docker compose exec npanel npanel make-admin npub1yourkeyhere
```

Point DNS at the box first — certificates are issued on the first visit.

## Quick start from a release

Releases are published over Nostr with [ngit](https://ngit.dev): the binaries,
`npanel-<version>-linux-x86_64` and `npanel-<version>-linux-aarch64`, and the
same as `.deb` and `.rpm` packages that set up the user and the systemd unit
(see [deployment](docs/deployment.md#from-a-package)).

```sh
chmod +x npanel-0.5.0-linux-x86_64

cp .env.example .env
$EDITOR .env          # set DASHBOARD_HOST at minimum

./npanel-0.5.0-linux-x86_64 make-admin npub1yourkeyhere
./npanel-0.5.0-linux-x86_64
```

To build it instead:

```sh
(cd dashboard && bun install && bun run build)
cargo build --release --features embedded-dashboard
```

Then put Caddy in front of it:

```sh
sudo cp Caddyfile /etc/caddy/Caddyfile
$EDITOR /etc/caddy/Caddyfile    # replace panel.example.com
sudo systemctl reload caddy
```

Visit your dashboard domain, log in with a Nostr extension, and add a site.

See [docs/deployment.md](docs/deployment.md) for a real production setup, either
way — DNS, TLS, backups, and a systemd unit.

## Commands

| Command                                         | Does                                          |
| ----------------------------------------------- | --------------------------------------------- |
| `npanel`                                        | Runs the gateway                              |
| `npanel make-admin <npub>`                      | Adds or promotes an admin                     |
| `npanel prune-blobs [--keep N] [--dry-run]`     | Runs the daily prune now                      |
| `npanel compress-blobs [--dry-run]`             | Stores compressed copies of older blobs       |

## Configuration

Seven environment variables, and that's the whole file:

| Variable          | Default     | Meaning                                     |
| ----------------- | ----------- | ------------------------------------------- |
| `PORT`            | `3000`      | Port the HTTP server listens on             |
| `BIND_ADDRESS`    | `127.0.0.1` | Address it listens on; only the proxy should reach it |
| `TLS_CHECK_PORT`  | `3001`      | Port answering the proxy's certificate questions, kept off the public one |
| `HTTPS_PORT`      | —           | Serve HTTPS here, with certificates from Let's Encrypt, instead of behind a proxy |
| `DASHBOARD_HOST`  | —           | Hostname that serves the dashboard and API  |
| `LOG_LEVEL`       | `info`      | `debug`, `info`, `warn`, or `error`         |
| `DATA_DIR`        | `./data`    | Holds the SQLite database and the blob store |

Relays, Blossom fallbacks, the file size limit, and the resync interval are
edited by admins in the dashboard, take effect without a restart, and are
recorded in the audit log. See [docs/configuration.md](docs/configuration.md).

## Documentation

- [Deployment](docs/deployment.md) — Docker, systemd, TLS, backups, upgrades,
  and the dashboard as an nsite
- [Configuration](docs/configuration.md) — env vars and runtime settings
- [Architecture](docs/architecture.md) — how sync, storage, and serving work
- [API](docs/api.md) — the JSON API behind the dashboard
- [Development](docs/development.md) — running npanel locally, and releasing it

## License

AGPL-3.0-or-later. `vendor/device-detector` is MIT, Matomo's device database in
`assets/device-detector` is LGPL-3.0, and Plausible's tracker in
`assets/analytics` is MIT; each carries its license beside it.
