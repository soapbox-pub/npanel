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
- **Analytics built in.** Flip a switch on a site and npanel counts its visits
  the way Plausible does — no cookies, nothing identifying stored — and shows
  them in a Plausible-style dashboard.

This is the Rust implementation of npanel, a port of the Bun/TypeScript one.
It opens the same data directory, speaks the
same API and serves the same dashboard, so it is a drop-in replacement — see
[moving from the TypeScript npanel](docs/deployment.md#moving-from-the-typescript-npanel).

## Performance

Measured side by side on one machine against a copy of the same data directory,
64 concurrent connections, 10 seconds per row:

| Request                               | Bun npanel | Rust npanel |
| ------------------------------------- | ---------: | ----------: |
| `index.html` (2 KB, brotli)           | 10,496/s   | 70,219/s    |
| JS bundle (555 KB, 149 KB brotli)     | 7,514/s    | 28,856/s    |
| Image (284 KB, identity)              | 5,643/s    | 15,756/s    |
| Range request (1 KB)                  | 9,418/s    | 71,157/s    |
| SPA fallback                          | 10,045/s   | 69,224/s    |
| 404                                   | 9,412/s    | 75,010/s    |
| 304 revalidation                      | 16,922/s   | 93,974/s    |
| `GET /api/instance`                   | 19,352/s   | 92,065/s    |
| Dashboard `index.html`                | 16,340/s   | 81,309/s    |

Median latency drops from 4–11 ms to 0.6–3.7 ms. The two large files are
limited by copying bytes over loopback rather than by npanel. Anonymous
memory after serving load and a burst of analytics events is 119 MB against
Bun's 171 MB; serving alone uses 35 MB. Host builds run two to four times
faster, and the analytics user-agent parser — a Matomo port vendored in
`vendor/device-detector` — parses in well under a millisecond.

The same comparison checked that the two agree: every file of every site was
requested from both with eight combinations of encoding, range, method and
conditional headers (14,894 requests), and every API route and 1,053 analytics
queries across three time zones were compared field by field. The differences
that remain are all places where Bun's HTTP layer departs from what the
TypeScript code asked for; they are listed in the deployment guide.

## Requirements

- Rust 1.85+ to build the server, and [Bun](https://bun.sh) 1.3+ to build the
  dashboard. At runtime npanel is one binary and the built dashboard.
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

## Quick start from source

```sh
cd npanel-rs
cargo build --release
(cd dashboard && bun install && bun run build)

cp .env.example .env
$EDITOR .env          # set DASHBOARD_HOST at minimum

./target/release/npanel make-admin npub1yourkeyhere
./target/release/npanel
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
| `npanel prune-blobs [--keep N] [--dry-run]`     | Forgets old versions and unreferenced blobs   |
| `npanel compress-blobs [--dry-run]`             | Stores compressed copies of older blobs       |

## Configuration

Six environment variables, and that's the whole file:

| Variable          | Default     | Meaning                                     |
| ----------------- | ----------- | ------------------------------------------- |
| `PORT`            | `3000`      | Port the HTTP server listens on             |
| `BIND_ADDRESS`    | `127.0.0.1` | Address it listens on; only the proxy should reach it |
| `TLS_CHECK_PORT`  | `3001`      | Port answering the proxy's certificate questions, kept off the public one |
| `DASHBOARD_HOST`  | —           | Hostname that serves the dashboard and API  |
| `LOG_LEVEL`       | `info`      | `debug`, `info`, `warn`, or `error`         |
| `DATA_DIR`        | `./data`    | Holds the SQLite database and the blob store |

Relays, Blossom fallbacks, the file size limit, and the resync interval are
edited by admins in the dashboard, take effect without a restart, and are
recorded in the audit log. See [docs/configuration.md](docs/configuration.md).

## Documentation

- [Deployment](docs/deployment.md) — Docker, systemd, TLS, backups, upgrades
- [Configuration](docs/configuration.md) — env vars and runtime settings
- [Architecture](docs/architecture.md) — how sync, storage, and serving work
- [API](docs/api.md) — the JSON API behind the dashboard
- [Development](docs/development.md) — running npanel locally

## License

AGPL-3.0-or-later. `vendor/device-detector` is MIT, Matomo's device database in
`assets/device-detector` is LGPL-3.0, and Plausible's tracker in
`assets/analytics` is MIT; each carries its license beside it.
