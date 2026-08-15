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

## Requirements

- [Bun](https://bun.sh) 1.3+ — the only runtime you need; Node.js is not required
- A reverse proxy for TLS — [Caddy](https://caddyserver.com) is the easy path,
  and a sample `Caddyfile` is included

## Quick start

```sh
cd npanel
bun install

cp .env.example .env
$EDITOR .env          # set DASHBOARD_HOST at minimum

bun run dashboard:install
bun run dashboard:build

bun run make-admin npub1yourkeyhere
bun start
```

Then put Caddy in front of it:

```sh
sudo cp Caddyfile /etc/caddy/Caddyfile
$EDITOR /etc/caddy/Caddyfile    # replace panel.example.com
sudo systemctl reload caddy
```

Visit your dashboard domain, log in with a Nostr extension, and add a site.

See [docs/deployment.md](docs/deployment.md) for a real production setup with a
systemd unit and TLS.

## Configuration

Five environment variables, and that's the whole file:

| Variable         | Default     | Meaning                                      |
| ---------------- | ----------- | -------------------------------------------- |
| `PORT`           | `3000`      | Port the HTTP server listens on              |
| `BIND_ADDRESS`   | `127.0.0.1` | Address it listens on; only the proxy should reach it |
| `DASHBOARD_HOST` | —           | Hostname that serves the dashboard and API   |
| `LOG_LEVEL`      | `info`      | `debug`, `info`, `warn`, or `error`          |
| `DATA_DIR`       | `./data`    | Holds the SQLite database and the blob store |

Relays, Blossom fallbacks, the file size limit, and the resync interval are
edited by admins in the dashboard, take effect without a restart, and are
recorded in the audit log. See [docs/configuration.md](docs/configuration.md).

## Documentation

- [Deployment](docs/deployment.md) — systemd, TLS, backups, upgrades
- [Configuration](docs/configuration.md) — env vars and runtime settings
- [Architecture](docs/architecture.md) — how sync, storage, and serving work
- [API](docs/api.md) — the JSON API behind the dashboard

## Development

```sh
bun run dev                 # server, with reload
bun run dashboard:dev       # dashboard, with HMR

bun lint                    # biome
bun check                   # tsc --noEmit
bun test                    # node:test, via bun
```

The dashboard has its own checks:

```sh
bun run dashboard:test      # tsc, eslint, vitest, and a production build
```

Run it from the root like that, or as `cd dashboard && bun run test`. Note the
`run`: a bare `bun test` in `dashboard/` would start Bun's own test runner
instead of the script, and the dashboard's tests need vitest's DOM environment.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
