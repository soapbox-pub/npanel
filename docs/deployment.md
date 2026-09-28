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
www.example.org.     A     203.0.113.10
blog.example.org.    A     203.0.113.10
```

A site on a root domain redirects its `www` to the bare name by default, and
that redirect needs `www` in DNS: it is served over HTTPS like anything else,
so the name has to reach this box and get a certificate before a browser can be
told where to go. Turn the redirect off per site in the dashboard if you'd
rather `www` stayed unresolved.

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
image fixes them: `PORT` is 3000, `TLS_CHECK_PORT` is 3001, `BIND_ADDRESS` is
`0.0.0.0`, and `DATA_DIR` is `/data` on a named volume. `LOG_LEVEL` is honoured
if you set it.

`0.0.0.0` looks like the opposite of the advice in
[configuration](configuration.md#bind_address), and is only safe because of what
compose does *not* do: nothing publishes either port, so both are reachable only
by Caddy over the project's internal network. Loopback inside the container would
mean reachable by nothing at all. Keep it that way — npanel speaks plain HTTP,
and 3001 authorizes certificates for anyone who asks.

Then bootstrap the first admin, as in [section 3](#3-bootstrap-the-first-admin),
inside the running container:

```sh
docker compose exec npanel bun run make-admin npub1yourkeyhere
```

`prune-blobs` and `compress-blobs` run the same way. All three are safe against
a running server: the database is in WAL mode.

Logs are the same JSON lines on stdout:

```sh
docker compose logs -f npanel | jq
```

Certificates work exactly as [section 5](#5-tls-with-caddy) describes — the
mounted `Caddyfile` is the same sample file, with `NPANEL_UPSTREAM` and
`NPANEL_TLS_CHECK` pointing at the container instead of at loopback.

### Using your own proxy

If you already run a proxy, or terminate TLS somewhere else, drop the `caddy`
service and publish npanel where only that proxy can reach it — loopback if it
runs on the host:

```yaml
services:
  npanel:
    ports:
      - "127.0.0.1:3000:3000"
      - "127.0.0.1:3001:3001" # only if your proxy asks about certificates
```

Leave the second line out unless the proxy does on-demand TLS. Nothing else
needs that port, and a proxy with a wildcard or manually issued certificate
never asks.

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

`tor-data` is Tor's own state and the control cookie it shares with npanel. It
is not worth backing up: onion keys live in npanel's database, so losing this
volume costs a fresh bootstrap and no addresses. See [Tor](#tor).

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
npanel provides the authorization endpoint Caddy needs — on `TLS_CHECK_PORT`,
which is a second listener, not a route on the public one. Copy the sample
`Caddyfile` from the repository root and replace `panel.example.com` — it is
written as `{$DASHBOARD_HOST:panel.example.com}` so that the same file can be
mounted into the Caddy container, but nothing stops you from writing your
hostname there directly:

```caddy
{
	on_demand_tls {
		ask http://{$NPANEL_TLS_CHECK:127.0.0.1:3001}/tls-check
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
	reverse_proxy {$NPANEL_UPSTREAM:127.0.0.1:3000}
}
```

The site block has no `encode`, and shouldn't. npanel compresses a site's files
once, when it downloads them, and picks the copy each visitor can read — so
there is nothing left for Caddy to compress, and compressing here would only
help the visitors who arrive through it. Visitors on a site's onion address
never touch Caddy at all, and they are the ones on the slowest connections.

The dashboard block keeps its `encode`: that's npanel's own built SPA served
off the local disk, not an nsite, so it isn't part of what gets compressed at
deploy time.

If you are upgrading an npanel that has already been running, compress what it
downloaded before you take `encode` out of the site block. Otherwise every site
it currently serves goes back to shipping raw HTML until it next deploys:

```sh
bun run compress-blobs
```

That is safe to re-run and skips anything already done; `--dry-run` reports how
much is outstanding without writing.

Caddy calls `/tls-check?domain=<name>` before issuing any certificate. npanel
answers `200` for the dashboard hostname, for every enabled host, and for the
`www` of a host that redirects it, and `404` for anything else. **Do not run on-demand TLS without the `ask` directive**:
anyone who points a domain at your IP could otherwise drive certificate issuance
on your behalf and get you rate-limited by Let's Encrypt.

Note the port: `3001`, not the `3000` everything else is proxied to. That
endpoint has to answer a request Caddy makes to the address it proxies to, with
no hostname in it that npanel could route on — so it cannot live on the public
listener, where it would have to answer on every site npanel serves. Published
like that, anyone could drive approvals for one of your hosts until its hourly
budget was gone and Caddy started refusing to renew. On its own port nothing
routes to it, and only the proxy can ask.

It is unauthenticated, necessarily so, and reveals only whether a hostname is
configured — which DNS already tells you. **Keep `TLS_CHECK_PORT` reachable only
by the proxy**, as `BIND_ADDRESS` does by default. Nothing else is served there.

Caddy has deprecated the `ask` sub-directive in favour of the `permission`
module. Newer versions accept:

```caddy
on_demand_tls {
	permission http http://127.0.0.1:3001/tls-check
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
use must **preserve the `Host` header**, because that is the only thing npanel
routes on. Caddy's `reverse_proxy` preserves it already; nginx's `proxy_pass`
rewrites it to the upstream address, so it needs telling:

```nginx
proxy_set_header Host $host;
proxy_set_header X-Forwarded-Proto $scheme;
```

Without the first, every hostname looks like the proxy's address and nothing
resolves. The second only matters if you terminate plain HTTP: npanel assumes
`https` when nothing says otherwise, so without it a plain-HTTP deployment
issues redirects to a scheme it doesn't serve.

`X-Forwarded-Host` is deliberately ignored, and setting it will not stand in
for the line above. npanel cannot tell a proxy's copy of that header from a
client's, and not every proxy overwrites it — nginx doesn't set it at all, so a
client's copy would pass straight through, and a visitor to any site could then
reach the dashboard and the whole JSON API by adding one header to a request.

That is one hole closed, not a licence to expose npanel: it still speaks plain
HTTP and still authorizes certificates without authentication, so keep it off
the internet. It listens on `127.0.0.1` unless you change `BIND_ADDRESS`; if
the proxy is on another machine, set `BIND_ADDRESS=0.0.0.0` and firewall the
port to it.

## Tor

Optional, and off until an admin turns it on. With
[`tor_enabled`](configuration.md#tor_enabled) set, every enabled site is also
published as a v3 onion service — one address per site, minted when the site is
added and withdrawn when it is removed. There is nothing to configure per site.

npanel needs a Tor daemon it can reach on a control port. It never edits
`torrc`: services are created over that port, and npanel keeps the key each
address is made of, which is what makes an address survive a restart of either
process.

### On Docker

Nothing to install — `compose.yaml` already runs a Tor container, built from
`tor/` in this repository. Turn the setting on under **Settings → Tor** and the
addresses appear within a second or two; each site's shows on its own page.

That container shares npanel's network namespace, which puts Tor's control port
on the same loopback address the default `tor_control_url` already names, and
keeps it off every network. The two containers also share a `tor-data` volume,
mounted read-only on npanel's side, because cookie authentication means npanel
has to read a file Tor writes — and it has to be at the same path in both, since
Tor names that path itself when asked how to authenticate.

One wart: sharing a namespace means `docker compose restart npanel` takes the
Tor container down with it. `restart: unless-stopped` brings it back on its own.

Onion keys are in npanel's database, not in `tor-data`, so losing that volume
costs a Tor bootstrap and nothing else. Back up `npanel-data` as always.

### Alongside systemd

```sh
sudo apt install tor
```

Distribution packages leave the control port off, so enable one in
`/etc/tor/torrc`:

```
ControlPort 9051
CookieAuthentication 1
CookieAuthFileGroupReadable 1
```

`ControlPort` on loopback is what the default `tor_control_url` expects, and
needs no change under Settings. Cookie authentication means the `npanel` user
must be able to read Tor's cookie, which on Debian and Ubuntu is the
`debian-tor` group:

```sh
sudo usermod -aG debian-tor npanel
sudo systemctl restart tor
sudo systemctl restart npanel
```

Both restarts matter — group membership is read at process start, so npanel
does not pick it up until it is restarted too.

Then order the unit after Tor, in `/etc/systemd/system/npanel.service`:

```ini
[Unit]
After=network-online.target tor.service
Wants=network-online.target tor.service
```

`Wants` rather than `Requires`: npanel starts and serves clearnet traffic
perfectly well with Tor down, retries the connection with backoff, and
republishes every address from its stored keys when Tor comes back. Tor being
unavailable should not keep the gateway from starting.

If you would rather use a `ControlSocket` than a TCP port, set
`tor_control_url` to `unix:/run/tor/control`. The hardening in [section
4](#4-systemd) does not get in the way: `ProtectSystem=strict` mounts the
hierarchy read-only, but connecting to a socket is not a write to the
filesystem it sits on, and no `ReadWritePaths=` is needed for it. Whether npanel
may connect is decided by the socket's own ownership and mode — which is the
`debian-tor` group above, exactly as for the cookie.

### Verifying

The Settings page reports whether npanel is connected and how many services it
has published, and names the error when it isn't. From the command line,
`/api/status` carries the same thing, and each host's `onion` field in
`/api/hosts` is its address.

To actually fetch a site over Tor you need a SOCKS proxy. The bundled container
deliberately has none (`SocksPort 0` — an open proxy on that network would be a
liability), so this works against a Tor installed from `apt` but not against
Docker:

```sh
curl --socks5-hostname 127.0.0.1:9050 http://<address>.onion/ -I
```

Otherwise, paste the address into Tor Browser. Visitors never need to be told
one: clearnet HTML responses carry an `Onion-Location` header, and Tor Browser
offers the onion on its own.

**Tor shows as not connected.** In order of likelihood: the control port isn't
enabled (`sudo ss -ltnp | grep 9051` finds nothing); npanel can't read the
cookie file, which logs a permission error and means the group above is missing
or npanel wasn't restarted after being added to it; `tor_control_url` points
somewhere Tor isn't; or Tor is guarded by a `HashedControlPassword`, in which
case put the password in
[`tor_control_password`](configuration.md#tor_control_password). npanel logs
each failed attempt and retries, so `journalctl -u npanel -f` shows the reason
without any need to restart anything.

**A site has no address yet.** Only enabled hosts get one, and publishing is
asynchronous — a newly added site takes a moment. If it never arrives, look for
`tor_onion_failed` in the log.

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
[The volumes](#the-volumes). Where there is no `sqlite3` binary at all, Bun can
take the same online backup with `VACUUM INTO`:

```sh
bun -e "import { Database } from 'bun:sqlite';
  new Database('/var/lib/npanel/npanel.sqlite3', { readonly: true })
    .exec(\"VACUUM INTO '/backup/npanel.sqlite3'\")"
```

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

An upgrade that changes what npanel builds for a site rebuilds every host at
startup, in the background. Each keeps serving its previous build until the new
one is ready — except on the upgrade that introduces builds, where there is no
previous one, and sites answer `503` for the few seconds until theirs lands.

## Reclaiming disk space

Every version of every site is kept, along with its files. On a site that
deploys often this adds up.

```sh
sudo -u npanel bun run prune-blobs --keep 10 --dry-run
sudo -u npanel bun run prune-blobs --keep 10
```

On Docker, `docker compose exec npanel bun run prune-blobs --keep 10`.

`--keep N` forgets all but the newest N versions of each site, then deletes
every blob no remaining version or host build references. The version a host is
currently serving and any pinned version are never removed, and neither is a
blob recorded in the last hour, which may belong to a build still in progress. Without `--keep` it only
deletes unreferenced blobs. `--dry-run` reports what would go.

A monthly timer is reasonable. Note that pruning a version also removes the
history you'd roll back to, so keep more than you think you need.

## Compressing what's already stored

npanel compresses a site's files as it downloads them, so this is only needed
once, on an npanel that was serving sites before it did that. Sites already
deployed have no reason to deploy again and would otherwise go on being served
raw indefinitely.

```sh
sudo -u npanel bun run compress-blobs --dry-run
sudo -u npanel bun run compress-blobs
```

On Docker, `docker compose exec npanel bun run compress-blobs`.

Run it before removing `encode` from the site block of your `Caddyfile`, not
after. It is safe to re-run and skips blobs that already have their copies, so
there is no harm in leaving it in a maintenance script.

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
`curl 'http://127.0.0.1:3001/tls-check?domain=example.org'` returns `ok`. A
`Caddyfile` still pointing `ask` at `:3000/api/tls-check` gets a `404` for every
name and denies them all — that endpoint moved to its own port.
