# Deployment

A production npanel is three pieces: DNS pointing at your box, npanel itself
running as a service on a local port, and a reverse proxy terminating TLS in
front of it.

DNS is the same either way. For the other two, pick one path: [Docker
Compose](#docker-compose), which brings up npanel and Caddy together, or
sections 2–5, which install the release binary under systemd.

The proxy is optional. npanel can terminate TLS itself, getting and renewing a
certificate for every site on its own — see [without a proxy](#without-a-proxy).

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
image fixes them: `PORT` is 3000, `TLS_CHECK_PORT` is 3001,
`BIND_ADDRESS` is `0.0.0.0`, and `DATA_DIR` is `/data` on a named volume. `LOG_LEVEL` is honoured
if you set it.

`0.0.0.0` looks like the opposite of the advice in
[configuration](configuration.md#bind_address), and is only safe because of what
compose does *not* do: nothing publishes any of its ports, so they are reachable
only by Caddy over the project's internal network. Loopback inside the container would
mean reachable by nothing at all. Keep it that way — npanel speaks plain HTTP,
and 3001 authorizes certificates for anyone who asks.

Then bootstrap the first admin, as in [section 3](#3-bootstrap-the-first-admin),
inside the running container:

```sh
docker compose exec npanel npanel make-admin npub1yourkeyhere
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

`npanel-data` is `DATA_DIR`: the database, the blob store, and Tor's state
once it is turned on. `caddy-data` holds
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

## 2. Install

npanel is one static binary, with the dashboard compiled in. Each release
publishes one for x86_64 Linux and one for aarch64, which run on any
distribution, alongside their `SHA256SUMS` and a sample systemd unit. Find them
under the repository's releases on
[gitworkshop.dev](https://gitworkshop.dev/alex@gleasonator.com/git.shakespeare.diy/npanel),
or with [ngit](https://ngit.dev) from a clone:

```sh
ngit release view npanel@0.3.1 --verify
```

The release is signed, and every file in it is addressed by its SHA-256, so any
Blossom server holding a copy serves the same bytes. Download
`npanel-<version>-linux-x86_64` (or `-aarch64`) and `npanel.service`, then:

```sh
sudo install -m 755 npanel-0.3.1-linux-x86_64 /usr/local/bin/npanel
sudo install -m 644 npanel.service /etc/systemd/system/npanel.service

sudo useradd --system --home /var/lib/npanel --shell /usr/sbin/nologin npanel
sudo install -d -o npanel -g npanel /var/lib/npanel
sudo install -d /etc/npanel
```

Nothing else is needed at runtime: the dashboard, the Matomo device database,
the Public Suffix List, the analytics tracker and the time zone data are all
inside the binary. The exception is fonts. Sites' link-preview scripts draw text
in the fonts installed on the system, so install a few, emoji included:

```sh
sudo apt install fonts-dejavu-core fonts-noto-color-emoji
```

Without any, previews draw no text, and npanel logs `link_preview_no_fonts`
when it starts.

Those processes are npanel's own binary, `npanel preview-worker`, started by
npanel and confined with Landlock and seccomp: they can't open a file, make a
socket or start a program. That needs nothing set up, but a container runtime
or kernel that refuses Landlock leaves only seccomp, and npanel logs
`link_preview_unconfined` saying so.

To add a font a site's script asks for, install its package if your
distribution has one — most popular Google Fonts do, like `fonts-roboto`,
`fonts-inter`, `fonts-open-sans` or `fonts-noto-cjk` — or put its `.ttf` or
`.otf` in `/usr/local/share/fonts/`. On Docker, build on the image and install
it there, or mount a directory of fonts at `/usr/local/share/fonts`. npanel
reads fonts as it starts the processes scripts run in, so restart it, then clear the cache under **Settings → Link
Previews**: an image already drawn doesn't change when the fonts do. The
distribution's emoji font trails Google's; for the newest emoji, put Google's
`NotoColorEmoji.ttf` in `/usr/local/share/fonts/` as well.

Configure. npanel reads `.env` from its working directory, which the unit sets
to `/etc/npanel`:

```sh
sudo $EDITOR /etc/npanel/.env
```

```
DASHBOARD_HOST="panel.example.com"
DATA_DIR="/var/lib/npanel"
```

`DASHBOARD_HOST` is the one you must set, and `DATA_DIR` has to match the
unit's `ReadWritePaths`. The rest have sensible defaults; see
[configuration](configuration.md) or `.env.example`.

### From source

On another architecture, or to run unreleased code, build it. You need
[rustup](https://rustup.rs), which installs the Rust `rust-toolchain.toml`
names, and [Bun](https://bun.sh) 1.3 or newer:

```sh
git clone <your-repo-url> npanel && cd npanel
(cd dashboard && bun install && bun run build)
cargo build --release --features embedded-dashboard
sudo install -m 755 target/release/npanel /usr/local/bin/npanel
sudo install -m 644 contrib/npanel.service /etc/systemd/system/npanel.service
```

and carry on from `useradd` above. The dashboard has to be built first:
`embedded-dashboard` compiles `dashboard/dist` into the binary and refuses to
build without it.

Leave the feature off and npanel reads the dashboard from disk instead —
`dashboard/dist` under its working directory, beside the binary, or in the
source tree it was built from — and picks up a rebuild without a restart. That
is what development wants; a server is better off with one file that can't
disagree with itself.

## 3. Bootstrap the first admin

npanel is not permissionless: an unknown pubkey gets a 403, and user management
in the dashboard is admin-only. So the first account has to come from the
command line, run where the unit runs npanel so that it finds the same `.env`:

```sh
cd /etc/npanel && sudo -u npanel npanel make-admin npub1yourkeyhere
```

It accepts an npub, nprofile, or hex pubkey, and promotes an existing user if
they're already there. Everyone after that is added from the Users page.

## 4. systemd

The release's `npanel.service`, also at `contrib/npanel.service` in the
repository, installed above as `/etc/systemd/system/npanel.service`:

```ini
[Unit]
Description=npanel
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=npanel
Group=npanel
WorkingDirectory=/etc/npanel
ExecStart=/usr/local/bin/npanel
Restart=on-failure
RestartSec=5s

# Logs are one JSON object per line on stdout.
StandardOutput=journal
StandardError=journal

# With HTTPS_PORT set, npanel binds 80 and 443 itself, which needs this.
#AmbientCapabilities=CAP_NET_BIND_SERVICE

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/npanel

[Install]
WantedBy=multi-user.target
```

Adjust `ReadWritePaths` if your `DATA_DIR` is elsewhere. `.env` is read from
the working directory, so `WorkingDirectory` is where the configuration lives.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now npanel
sudo journalctl -u npanel -f -o cat | jq
```

An npanel installed from a checkout by an earlier version of this guide runs
`/opt/npanel/target/release/npanel` with `/opt/npanel` as its working
directory. That keeps working; to move to the release binary, copy its `.env`
to `/etc/npanel/`, install the new unit, and restart.

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

Analytics events need no route of their own. Every site that counts visits
reports to `/_npanel/api/event` on its own origin, and npanel hands each one to
its analytics thread from the listener that received it. The proxy must set
`X-Forwarded-For` to the visitor's address, which Caddy does; it is how
visitors are told apart and placed, and is never stored. An event names the
site it is for, but is only recorded for the site whose hostname it was sent
to.

An older Caddyfile has a `reverse_proxy /.npanel/api/event` line pointing at a
third port, `3002`. Take it out when you upgrade: nothing listens there any
more, and every event would fail with a `502`.

The site block has no `encode`, and shouldn't. npanel compresses a site's files
once, when it downloads them, and picks the copy each visitor can read — so
there is nothing left for Caddy to compress, and compressing here would only
help the visitors who arrive through it. Visitors on a site's onion address
never touch Caddy at all, and they are the ones on the slowest connections.

The dashboard block keeps its `encode`, for the API's JSON. The dashboard's
own files npanel compresses itself, when it starts, and Caddy passes those
through as they are.

If you are upgrading an npanel that has already been running, compress what it
downloaded before you take `encode` out of the site block. Otherwise every site
it currently serves goes back to shipping raw HTML until it next deploys:

```sh
npanel compress-blobs
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

## Without a proxy

npanel can serve HTTPS itself, with a certificate from Let's Encrypt for every
name it serves: the dashboard, each enabled site, and the `www` of each site
that redirects it. Each is ordered as soon as the name is added and renewed
with a third of its lifetime left, and nothing restarts for either. Set
`HTTPS_PORT` in `.env`:

```
DASHBOARD_HOST="panel.example.com"
DATA_DIR="/var/lib/npanel"
HTTPS_PORT="443"
PORT="80"
```

Port 443 serves everything, over HTTP/2 or HTTP/1.1. Port 80 answers the
certificate authority's HTTP-01 challenges and sends everything else to HTTPS;
nothing is served in the clear. `BIND_ADDRESS` defaults to every address, IPv6
and IPv4 both, and `TLS_CHECK_PORT` isn't used. Both ports must be open to the
internet: the certificate authority validates over 443 (TLS-ALPN-01) and 80
(HTTP-01), alternating from one attempt to the next.

**Setting `HTTPS_PORT` agrees to the certificate authority's terms of
service** — [Let's Encrypt's](https://letsencrypt.org/repository/), by default.

Binding ports below 1024 needs a capability the `npanel` user doesn't have.
The sample unit carries the line, commented out; uncomment it:

```ini
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

then `sudo systemctl daemon-reload && sudo systemctl restart npanel`. Stop any
web server already holding 80 or 443 first.

On Docker, drop the `caddy` service and publish npanel's ports instead. Docker
lets a container's own user bind low ports, so nothing else is needed:

```yaml
services:
  npanel:
    environment:
      HTTPS_PORT: "443"
      PORT: "80"
    ports:
      - "80:80"
      - "443:443"
```

Keep the published ports the same as the container's: the redirect to HTTPS
names `HTTPS_PORT`, so `443:8443` would send visitors to `:8443`.

DNS works as [section 1](#1-dns) describes, and has to point here before a site
is added: npanel orders the certificate at once, and a name the certificate
authority can't reach here fails. It isn't stuck — npanel tries again after two
minutes, then four, and so on up to once an hour, with the challenge it didn't
use last time — but fixing DNS promptly gets the certificate sooner. A site's
page in the dashboard shows its certificates, and the reason for any failure;
**Settings → Certificates** lists every name that is failing.

Try a new setup against Let's Encrypt's staging CA first, with the switch under
**Settings → Certificates**: its certificates aren't trusted by browsers, but
its limits are far higher, so a mistake costs nothing. Switch back once
everything validates.

Certificates, their keys, and the ACME account are kept in the database, so a
[database backup](#backups) covers them, and a restart serves what is stored
without asking for anything. The backoff a failing name has earned is stored
too, so restarting doesn't start it over.

Without a proxy, nothing in a request's headers is taken from one:
`X-Forwarded-For` and `X-Forwarded-Proto` are ignored, analytics count the
address that connected, and the scheme is `https`.

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

Tor is built into npanel, so there is nothing to install, on Docker or under
systemd. Turn the setting on under **Settings → Tor**. npanel joins the Tor
network, which takes a minute or so the first time, and the addresses answer
once it has; each site's shows on its own page.

npanel keeps the key each address is made of in its database, which is what
makes an address survive a restart. Tor's own state — its directory cache and
its guards — goes in `DATA_DIR/tor`. It is not
worth backing up on its own: losing it costs a slower start and no addresses.

Tor checks the permissions of the directories it writes to, and refuses to
start in one that another user could change: `DATA_DIR/tor`, and every
directory above it, must not be writable by anyone but npanel's user and root.
The layouts in this guide already are. If Tor won't start, the Settings page
and the log (`tor_unavailable`) both say why.

Upgrading from a version that drove a separate Tor daemon needs nothing: every
site keeps its address. The `tor_control_url` and `tor_control_password`
settings are gone. The daemon can be removed — on Docker, `docker compose up -d
--remove-orphans` takes down the old `tor` container, after which the
`npanel_tor-data` volume can be deleted; alongside systemd, the `ControlPort`
lines, the `debian-tor` group membership, and the `tor.service` ordering in the
unit can all go.

### Verifying

The Settings page reports whether npanel is on the Tor network and how many
services it is running, and names the problem when something is wrong. From the
command line, `/api/status` carries the same thing, and each host's `onion`
field in `/api/hosts` is its address.

To actually fetch a site over Tor you need a Tor client with a SOCKS proxy, on
any machine; npanel itself runs none:

```sh
curl --socks5-hostname 127.0.0.1:9050 http://<address>.onion/ -I
```

Otherwise, paste the address into Tor Browser. Visitors never need to be told
one: clearnet HTML responses carry an `Onion-Location` header, and Tor Browser
offers the onion on its own. A newly published address can take a minute or two
to become reachable while its descriptor spreads through the network.

**Tor's own messages** are in npanel's log as `tor_log` entries. Its warnings
are always there; set `LOG_LEVEL=debug` to see the rest of what it is doing.

**Tor is stuck joining the network.** The Settings page shows the reason Tor
gives. npanel needs to make outbound connections to Tor relays, on whatever
ports they listen on, so a firewall that only allows outbound HTTPS will stop
it. A clock that is far wrong also stops it, since Tor checks the dates on what
relays send. npanel keeps trying, so fixing either needs no restart.

**Limits worth knowing.** Arti, the Tor implementation built in, is younger
than the C Tor most onion services run on. Its proof-of-work defence against
floods of connection attempts is still marked experimental, so npanel's build
leaves it out, and its onion services go without what C Tor offers there. A
gateway whose onions come under that kind of attack can turn Tor off without
affecting its clearnet sites.

**A site has no address yet.** Only enabled hosts get one, and publishing is
asynchronous — a newly added site takes a moment. If it never arrives, look for
`tor_onion_failed` in the log.

## The dashboard as an nsite

Every release also publishes its dashboard as a named nsite, `npanel`, signed by
the same key as the release. It is the same build the binary carries, and it can
manage any npanel server: opened anywhere but on a server's own dashboard host,
it asks for the server's address, remembers it, and talks to the API across
origins. NIP-98 signs each request for the server's URL, so the server needs no
configuration for it.

Nothing about a server changes, and its own dashboard keeps working; this is a
second way in. It suits someone who manages several servers, or who would
rather load the dashboard from a gateway they trust than from the server it
manages.

The nsite follows the latest release, and a server may be behind it. A server
older than the dashboard gets a notice saying so, since a page built for a newer
API may fail against it. Servers before 0.2.0 don't report a version, and are
always flagged.

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
named by hash, so incremental syncs are cheap. `previews.sqlite3` is a cache of
what relays said, and needn't be backed up at all.

## Upgrading

Install the new release's binary over the old one and restart:

```sh
sudo install -m 755 npanel-0.3.1-linux-x86_64 /usr/local/bin/npanel
sudo systemctl restart npanel
```

From source, `git pull`, rebuild the dashboard and the binary as in
[section 2](#from-source), and install it the same way.

Migrations run automatically at startup. Back up the database first. On Docker
this is [`docker compose up -d --build`](#upgrading-the-containers) instead.

An upgrade that changes what npanel builds for a site rebuilds every host at
startup, in the background. Each keeps serving its previous build until the new
one is ready — except on the upgrade that introduces builds, where there is no
previous one, and sites answer `503` for the few seconds until theirs lands.

## Reclaiming disk space

npanel does this by itself. Once a day it keeps the newest 20 ready versions of
each site, forgets versions of sites no host points at, and deletes every
stored file nothing uses any more. **Settings → Deploys → Versions kept per
site** changes how many; `0` keeps every version. **Settings** shows how much
is waiting to be cleaned up and what the last cleanup freed. See
[`keep_versions`](configuration.md#keep_versions).

To clean up now rather than wait for the day's run:

```sh
cd /etc/npanel
sudo -u npanel npanel prune-blobs --dry-run
sudo -u npanel npanel prune-blobs
```

On Docker, `docker compose exec npanel npanel prune-blobs`.

It applies the setting, or `--keep N` in its place. The version a host is
serving and any pinned version are never removed, and neither is a file
recorded in the last hour, which may belong to a build still in progress.
`--dry-run` reports what would go, though not the files that forgetting
versions would then free, since it forgets nothing. It is safe to run while
the server is.

## Compressing what's already stored

npanel compresses a site's files as it downloads them, so this is only needed
once, on an npanel that was serving sites before it did that. Sites already
deployed have no reason to deploy again and would otherwise go on being served
raw indefinitely.

```sh
cd /etc/npanel
sudo -u npanel npanel compress-blobs --dry-run
sudo -u npanel npanel compress-blobs
```

On Docker, `docker compose exec npanel npanel compress-blobs`.

Run it before removing `encode` from the site block of your `Caddyfile`, not
after. It is safe to re-run and skips blobs that already have their copies, so
there is no harm in leaving it in a maintenance script.

## Troubleshooting

**The dashboard returns 503 "The dashboard has not been built."** Only a
binary built without `embedded-dashboard` can say this, and it means it found
no `dashboard/dist`. Run `bun install && bun run build` in `dashboard/`. The
server logs `dashboard_not_built` once with the directories it looked in. The
image builds it, so on Docker this means the container is running a stale image
— `docker compose up -d --build`.

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

**A site has no certificate, with `HTTPS_PORT` set.** Its page in the dashboard
says why, in the certificate authority's words, and the log has it as
`tls_certificate_failed`. Usually DNS doesn't point here yet, or a firewall
blocks port 80 or 443. npanel keeps trying on its own. A browser connecting
before there is one gets a TLS error rather than someone else's certificate.

**Caddy won't issue a certificate.** Confirm DNS resolves to the box, the host
exists in the dashboard and is enabled, and that
`curl 'http://127.0.0.1:3001/tls-check?domain=example.org'` returns `ok`. A
`Caddyfile` still pointing `ask` at `:3000/api/tls-check` gets a `404` for every
name and denies them all — that endpoint moved to its own port.
