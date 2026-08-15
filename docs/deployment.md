# Deployment

A production npanel is three pieces: DNS pointing at your box, npanel itself
running as a service on a local port, and a reverse proxy terminating TLS in
front of it.

DNS is the same either way. For the other two, pick one path: [Docker
Compose](#docker-compose), which brings up npanel and Caddy together, or
sections 2–5, which install from a checkout under systemd.

## 1. DNS

Point every hostname you plan to serve at the box:

```
panel.example.com.   A     203.0.113.10
panel.example.com.   AAAA  2001:db8::10
example.org.         A     203.0.113.10
blog.example.org.    A     203.0.113.10
```

The dashboard hostname is just another name pointed at the same address. A
wildcard (`*.example.com`) works too, and is convenient when staff add
subdomains often — npanel still refuses to serve, or to get a certificate for,
any hostname that isn't configured in the dashboard.

DNS must resolve *before* you add a host, because the certificate is issued the
first time someone visits.

The dashboard shows staff exactly which records to create, working them out from
what the dashboard hostname itself resolves to. If that inference is wrong for
your setup — a CDN in front, or a CNAME you'd rather everyone used — set
[`dns_targets`](configuration.md#dns_targets).

## Docker Compose

This replaces sections 2–5: `compose.yaml` runs npanel from an image built out
of the repository, with Caddy in front of it for TLS.

```sh
git clone <your-repo-url> npanel
cd npanel

cp .env.example .env
$EDITOR .env          # DASHBOARD_HOST is the only one that applies here

docker compose up -d
```

Compose reads `DASHBOARD_HOST` from `.env` and gives it to both containers —
npanel to route on, Caddy as the name of the dashboard's site. It refuses to
start without it. The other variables in that file don't apply, because the
image fixes them: `PORT` is 3000, `BIND_ADDRESS` is `0.0.0.0`, and `DATA_DIR` is
`/data` on a named volume. `LOG_LEVEL` is honoured if you set it.

`0.0.0.0` looks like the opposite of the advice in
[configuration](configuration.md#bind_address), and is only safe because of what
compose does *not* do: nothing publishes npanel's port, so it is reachable only
by Caddy over the project's internal network. Loopback inside the container
would mean reachable by nothing at all. Keep it that way — npanel trusts
`X-Forwarded-Host` from whoever connects and answers `/api/tls-check` without
authentication.

Then bootstrap the first admin, as in [section 3](#3-bootstrap-the-first-admin),
inside the running container:

```sh
docker compose exec npanel bun run make-admin npub1yourkeyhere
```

`prune-blobs` runs the same way. Both are safe against a running server: the
database is in WAL mode.

Logs are the same JSON lines on stdout:

```sh
docker compose logs -f npanel | jq
```

Certificates work exactly as [section 5](#5-tls-with-caddy) describes — the
mounted `Caddyfile` is the same sample file, with `NPANEL_UPSTREAM` set to
`npanel:3000` instead of its loopback default.

### Using your own proxy

If you already run a proxy, or terminate TLS somewhere else, drop the `caddy`
service and publish npanel where only that proxy can reach it — loopback if it
runs on the host:

```yaml
services:
  npanel:
    ports:
      - "127.0.0.1:3000:3000"
```

[Other proxies](#other-proxies) covers the one header it has to set.

### Upgrading the containers

```sh
git pull
docker compose up -d --build
```

Migrations run at startup, so back up the database first.

### The volumes

`npanel-data` is `DATA_DIR`: the database and the blob store. `caddy-data` holds
the certificates and the ACME account key — losing it means re-issuing every
certificate, which Let's Encrypt rate limits, so it is worth backing up even
though it rebuilds itself.

The image has no `sqlite3` binary, so take the backup from
[Backups](#backups) with a throwaway container that does. It's safe while npanel
is running:

```sh
docker run --rm -v npanel_npanel-data:/data -v "$PWD:/backup" alpine:3 sh -c \
  'apk add --no-cache sqlite >/dev/null && \
   sqlite3 /data/npanel.sqlite3 ".backup /backup/npanel-$(date +%F).sqlite3"'
```

## 2. Install and build

npanel runs from a checkout. Bun runs the TypeScript directly; there is no build
step for the server.

```sh
sudo useradd --system --home /opt/npanel --shell /usr/sbin/nologin npanel
sudo mkdir -p /opt/npanel
sudo chown npanel:npanel /opt/npanel

sudo -u npanel git clone <your-repo-url> /opt/npanel
cd /opt/npanel
sudo -u npanel bun install
```

Build the dashboard. The server serves `dashboard/dist` as a static SPA, so this
must exist or the dashboard hostname returns 503 with an explanatory message:

```sh
sudo -u npanel bun run dashboard:install
sudo -u npanel bun run dashboard:build
```

Configure:

```sh
sudo -u npanel cp .env.example .env
sudo -u npanel $EDITOR .env
```

At minimum set `DASHBOARD_HOST`. Set `DATA_DIR` to somewhere durable —
`/var/lib/npanel` is a good choice on a machine where the checkout might be
replaced:

```sh
sudo mkdir -p /var/lib/npanel
sudo chown npanel:npanel /var/lib/npanel
```

## 3. Bootstrap the first admin

npanel is not permissionless: an unknown pubkey gets a 403, and user management
in the dashboard is admin-only. So the first account has to come from the
command line.

```sh
sudo -u npanel bun run make-admin npub1yourkeyhere
```

It accepts an npub, nprofile, or hex pubkey, and promotes an existing user if
they're already there. Everyone after that is added from the Users page.

## 4. systemd

`/etc/systemd/system/npanel.service`:

```ini
[Unit]
Description=npanel
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=npanel
Group=npanel
WorkingDirectory=/opt/npanel
ExecStart=/usr/local/bin/bun run src/server.ts
Restart=on-failure
RestartSec=5s

# Logs are one JSON object per line on stdout.
StandardOutput=journal
StandardError=journal

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/npanel

[Install]
WantedBy=multi-user.target
```

Adjust `ExecStart` to wherever `bun` actually lives (`command -v bun`), and
`ReadWritePaths` to your `DATA_DIR`.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now npanel
sudo journalctl -u npanel -f -o cat | jq
```

## 5. TLS with Caddy

npanel speaks plain HTTP and routes on the `Host` header. It never terminates
TLS itself.

The wrinkle is that staff add hostnames at runtime, so the set of certificates
isn't known when the proxy starts. Caddy solves this with on-demand TLS, and
npanel provides the authorization endpoint Caddy needs. Copy the sample
`Caddyfile` from the repository root and replace `panel.example.com` — it is
written as `{$DASHBOARD_HOST:panel.example.com}` so that the same file can be
mounted into the Caddy container, but nothing stops you from writing your
hostname there directly:

```caddy
{
	on_demand_tls {
		ask http://{$NPANEL_UPSTREAM:127.0.0.1:3000}/api/tls-check
	}
}

{$DASHBOARD_HOST:panel.example.com} {
	encode zstd gzip
	reverse_proxy {$NPANEL_UPSTREAM:127.0.0.1:3000}
}

https:// {
	tls {
		on_demand
	}
	encode zstd gzip
	reverse_proxy {$NPANEL_UPSTREAM:127.0.0.1:3000}
}
```

`encode` is not optional in practice. npanel serves every blob exactly as it was
uploaded and never compresses anything, so without it a site's HTML and JS go
out raw — for a typical SPA bundle that is several megabytes instead of a few
hundred kilobytes. Caddy's defaults already skip types that are compressed
already, like images and video.

Caddy calls `/api/tls-check?domain=<name>` before issuing any certificate.
npanel answers `200` for the dashboard hostname and for every enabled host, and
`404` for anything else. **Do not run on-demand TLS without the `ask`
directive**: anyone who points a domain at your IP could otherwise drive
certificate issuance on your behalf and get you rate-limited by Let's Encrypt.

That endpoint is the one unauthenticated route in the API, necessarily so — it
only reveals whether a hostname is configured, which DNS already tells you.

It is also the one route exempt from npanel's `Host` routing. Caddy asks with a
plain `GET` to the address it proxies to, so the request arrives with
`Host: 127.0.0.1:3000` rather than the dashboard hostname; were it routed like
everything else it would be served as an unconfigured nsite, answer `404`, and
quietly deny every certificate. Because of that exemption `/api/tls-check`
answers on any hostname npanel serves — keep it on a port only the proxy can
reach, as the sample configuration does.

Caddy has deprecated the `ask` sub-directive in favour of the `permission`
module. Newer versions accept:

```caddy
on_demand_tls {
	permission http http://127.0.0.1:3000/api/tls-check
}
```

Both forms send the same request; use whichever your Caddy accepts.

Caddy has no rate limiting of its own on this path — the `ask` endpoint is meant
to be the throttle — so npanel meters approvals: 60 per hostname per hour and
600 overall by default, both editable under Settings. Past that it answers `429`
and Caddy declines to issue. This matters for hosts whose DNS doesn't actually
point here: each handshake asks again, each `ok` starts an order that fails
validation, and anyone can drive that loop by connecting with the name as SNI.
Refusals cost nothing, and the dashboard hostname is never throttled. See
[configuration](configuration.md#tls_check_limit_per_host).

### Other proxies

nginx, HAProxy, and friends work, but you'll be managing certificates yourself
(or with a wildcard cert) since they have no on-demand equivalent. Whatever you
use must either preserve the `Host` header or set `X-Forwarded-Host`; npanel
prefers `X-Forwarded-Host` when present. Without one of those, every hostname
looks like the proxy's address and nothing resolves.

npanel trusts `X-Forwarded-Host` unconditionally, because it has no way to tell
a proxy's header from a client's. Your proxy must therefore *set* that header
rather than pass a client-supplied one through — Caddy and nginx's
`proxy_set_header` both do — and npanel must not be reachable directly from the
internet. It listens on `127.0.0.1` unless you change `BIND_ADDRESS`; if the
proxy is on another machine, set `BIND_ADDRESS=0.0.0.0` and firewall the port
to it.

## Adding a site

In the dashboard: **Add site**, give it a domain and an nsite address. The
address can be an npub, nprofile, naddr, an `a` tag
(`35128:<pubkey>:<identifier>`), a NIP-5A label, or a URL to paste from.

npanel then fetches the newest manifest, downloads every blob it references, and
only marks the version ready when all of them are on disk. Until then the host
serves `503` with a `Retry-After`. If a deploy fails, open the version's deploy
log — it records each blob attempt, the servers tried, and why it gave up.

## Backups

Everything is in `DATA_DIR`. The blob store is content-addressed and re-fetchable
from Blossom servers, so the database is the part that actually matters — losing
blobs costs a re-download, losing the database costs your configuration.

Back up SQLite with the online backup API rather than copying the file, which
can catch a write in progress:

```sh
sqlite3 /var/lib/npanel/npanel.sqlite3 ".backup '/backup/npanel-$(date +%F).sqlite3'"
```

On Docker the same call runs in a throwaway container against the volume — see
[The volumes](#the-volumes).

To back up blobs too, `rsync` `DATA_DIR/blobs` — files there are immutable and
named by hash, so incremental syncs are cheap.

## Upgrading

```sh
cd /opt/npanel
sudo -u npanel git pull
sudo -u npanel bun install
sudo -u npanel bun run dashboard:install
sudo -u npanel bun run dashboard:build
sudo systemctl restart npanel
```

Migrations run automatically at startup. Back up the database first. On Docker
this is [`docker compose up -d --build`](#upgrading-the-containers) instead.

## Reclaiming disk space

Every version of every site is kept, along with its files. On a site that
deploys often this adds up.

```sh
sudo -u npanel bun run prune-blobs --keep 10 --dry-run
sudo -u npanel bun run prune-blobs --keep 10
```

On Docker, `docker compose exec npanel bun run prune-blobs --keep 10`.

`--keep N` forgets all but the newest N versions of each site, then deletes
every blob no remaining version references. The version a host is currently
serving and any pinned version are never removed. Without `--keep` it only
deletes unreferenced blobs. `--dry-run` reports what would go.

A monthly timer is reasonable. Note that pruning a version also removes the
history you'd roll back to, so keep more than you think you need.

## Troubleshooting

**The dashboard returns 503 "The dashboard has not been built."** Run
`bun run dashboard:build`. The server logs `dashboard_not_built` once with the
directory it looked in. The image builds it, so on Docker this means the
container is running a stale image — `docker compose up -d --build`.

**A site returns 503 "This site has not finished deploying yet."** No version is
ready. Check the version list and its deploy log in the dashboard.

**Login fails with "Auth event was signed for … not …".** `DASHBOARD_HOST`
doesn't match the hostname in the browser. It must be the public name, not the
proxy's upstream address.

**Login fails with "Auth event expired."** The clock on the server or the client
is off by more than a minute. Run NTP.

**Login fails with "This account has no access to npanel."** The signature is
valid but the pubkey isn't a user. Add it from the Users page, or bootstrap with
`make-admin`.

**Caddy won't issue a certificate.** Confirm DNS resolves to the box, the host
exists in the dashboard and is enabled, and that
`curl 'http://127.0.0.1:3000/api/tls-check?domain=example.org'` returns `ok`.
