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

echo 'DASHBOARD_HOST=panel.example.com' > .env

docker compose up -d
```

That `.env` is compose's own, read by compose rather than npanel. Compose
gives `DASHBOARD_HOST` to both containers — to npanel as `dashboard_host` in
the config file it writes for it (the `configs` block at the end of
`compose.yaml`), to Caddy as the name of the dashboard's site — and refuses to
start without it. `LOG_LEVEL` sets npanel's `log_level` if you add it. The rest
of that config file is fixed for the container: `port` is 3000,
`tls_check_port` is 3001, `bind_address` is `"0.0.0.0"`, and `data_dir` is
`/data` on a named volume. To set anything else from
[configuration](configuration.md#the-config-file), add it to that block.

`"0.0.0.0"` looks like the opposite of the advice in
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

`npanel-data` is `data_dir`: the database, the blob store, and Tor's state
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
distribution, and the same binaries as `.deb` and `.rpm` packages. Find them
under the repository's releases on
[gitworkshop.dev](https://gitworkshop.dev/soapbox.pub/npanel),
or with [ngit](https://ngit.dev) from a clone:

```sh
ngit release view npanel@0.6.2 --verify
```

The release is signed, and names every file in it by its SHA-256, which
`--verify` checks; any Blossom server holding a copy serves the same bytes.

### From a package

On Debian, Ubuntu, Fedora, RHEL and their relatives, install the package for
your architecture:

```sh
sudo apt install ./npanel-0.6.2-linux-x86_64.deb    # or
sudo dnf install ./npanel-0.6.2-linux-x86_64.rpm
```

It does everything the rest of this section and [section 4](#4-systemd) do by
hand, with the binary at `/usr/bin/npanel` rather than `/usr/local/bin`: the
`npanel` user, `/var/lib/npanel`, the unit, the fonts below (as recommended
packages), and `/etc/npanel/npanel.toml`, which is `npanel.example.toml` with
`data_dir` set and `dashboard_host` empty. Set `dashboard_host` there, then
start it:

```sh
sudo $EDITOR /etc/npanel/npanel.toml
sudo systemctl enable --now npanel
```

The service is left stopped and disabled until then, since it refuses to start
without `dashboard_host`. An upgrade keeps your `npanel.toml` and restarts
npanel if it was running. Removing the package stops and disables it but keeps
`/var/lib/npanel` and the `npanel` user; purging it also removes
`npanel.toml`. For
`AmbientCapabilities` or another change to the unit, use `sudo systemctl edit
npanel` rather than editing it, so the next upgrade doesn't undo it.

Moving from an install by hand, remove `/etc/systemd/system/npanel.service` and
`/usr/local/bin/npanel` after installing the package: that unit overrides the
package's and runs the old binary. Move the settings in `/etc/npanel/.env`
into `npanel.toml` as in [Upgrading](#from-env-to-npaneltoml), then delete
`.env`.

Then carry on from [section 3](#3-bootstrap-the-first-admin).

### By hand

Download `npanel-<version>-linux-x86_64` (or `-aarch64`), and
[`contrib/npanel.service`](../contrib/npanel.service) and
[`npanel.example.toml`](../npanel.example.toml) from the repository at the
release's tag, then:

```sh
sudo install -m 755 npanel-0.6.2-linux-x86_64 /usr/local/bin/npanel
sudo install -m 644 npanel.service /etc/systemd/system/npanel.service

sudo useradd --system --home /var/lib/npanel --shell /usr/sbin/nologin npanel
sudo install -d -o npanel -g npanel /var/lib/npanel
sudo install -D -m 644 npanel.example.toml /etc/npanel/npanel.toml
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

Configure. The unit runs npanel with `--config /etc/npanel/npanel.toml`:

```sh
sudo $EDITOR /etc/npanel/npanel.toml
```

```toml
dashboard_host = "panel.example.com"
data_dir = "/var/lib/npanel"
```

`dashboard_host` is the one you must set, and `data_dir` has to match the
unit's `ReadWritePaths`: the default for a user other than root is under its
home directory. The rest have sensible defaults; the example file lists them,
and [configuration](configuration.md) explains each.

### From source

To run unreleased code, build it the way a release is built: the same script,
which makes the same static binary with the dashboard compiled in. You need
[rustup](https://rustup.rs), which installs the Rust `rust-toolchain.toml`
names, and [Bun](https://bun.sh) 1.3 or newer; `contrib/release/tools.sh`
fetches the rest, on an x86_64 or aarch64 machine, and either builds both:

```sh
git clone <your-repo-url> npanel && cd npanel
(cd dashboard && bun install && bun run build)
export PATH="$(contrib/release/tools.sh ~/.cache/npanel-release-tools):$PATH"
contrib/release/build.sh dist x86_64           # or aarch64
sudo install -m 755 dist/npanel-0.6.2-linux-x86_64 /usr/local/bin/npanel
sudo install -m 644 contrib/npanel.service /etc/systemd/system/npanel.service
```

and carry on from `useradd` above. A plain `cargo build --release` is not the
same thing: without the `embedded-dashboard` feature npanel reads the dashboard
from `dashboard/dist` on disk, which is what development wants and a server
doesn't — one file can't disagree with itself.

## 3. Bootstrap the first admin

npanel is not permissionless: an unknown pubkey gets a 403, and user management
in the dashboard is admin-only. So the first account has to come from the
command line, as the `npanel` user so that it can write to the database. It
finds `/etc/npanel/npanel.toml` as the service does:

```sh
sudo -u npanel npanel make-admin npub1yourkeyhere
```

It accepts an npub, nprofile, or hex pubkey, and promotes an existing user if
they're already there. Everyone after that is added from the Users page.

## 4. systemd

`contrib/npanel.service` in the repository, installed above as
`/etc/systemd/system/npanel.service`. The packages install it as
`/usr/lib/systemd/system/npanel.service`, running `/usr/bin/npanel`.

```ini
[Unit]
Description=npanel
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=npanel
Group=npanel
ExecStart=/usr/local/bin/npanel --config /etc/npanel/npanel.toml
Restart=on-failure
RestartSec=5s

# Logs are one JSON object per line on stdout.
StandardOutput=journal
StandardError=journal

# With https_port set, npanel binds 80 and 443 itself, which needs this.
#AmbientCapabilities=CAP_NET_BIND_SERVICE

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/npanel

[Install]
WantedBy=multi-user.target
```

Adjust `ReadWritePaths` if your `data_dir` is elsewhere. The configuration is
named outright rather than searched for, since `ProtectHome` hides the
`~/.config` npanel would otherwise look in first.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now npanel
sudo journalctl -u npanel -f -o cat | jq
```

An npanel installed from a checkout by an earlier version of this guide runs
`/opt/npanel/target/release/npanel` with `/opt/npanel` as its working
directory, and its configuration in `/opt/npanel/.env`. Write that
configuration to `/etc/npanel/npanel.toml` as in
[Upgrading](#from-env-to-npaneltoml), install the release binary and the new
unit, and restart.

## 5. TLS with Caddy

npanel speaks plain HTTP and routes on the `Host` header. It never terminates
TLS itself.

The wrinkle is that staff add hostnames at runtime, so the set of certificates
isn't known when the proxy starts. Caddy solves this with on-demand TLS, and
npanel provides the authorization endpoint Caddy needs — on `tls_check_port`,
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
configured — which DNS already tells you. **Keep `tls_check_port` reachable only
by the proxy**, as `bind_address` does by default. Nothing else is served there.

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
`https_port` in `npanel.toml`:

```toml
dashboard_host = "panel.example.com"
data_dir = "/var/lib/npanel"
https_port = 443
port = 80
```

Port 443 serves everything, over HTTP/2 or HTTP/1.1. Port 80 answers the
certificate authority's HTTP-01 challenges and sends everything else to HTTPS;
nothing is served in the clear. `bind_address` defaults to every address, IPv6
and IPv4 both, and `tls_check_port` isn't used. Both ports must be open to the
internet: the certificate authority validates over 443 (TLS-ALPN-01) and 80
(HTTP-01), alternating from one attempt to the next.

**Setting `https_port` agrees to the certificate authority's terms of
service** — [Let's Encrypt's](https://letsencrypt.org/repository/), by default.

Binding ports below 1024 needs a capability the `npanel` user doesn't have.
The sample unit carries the line, commented out; uncomment it:

```ini
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

then `sudo systemctl daemon-reload && sudo systemctl restart npanel`. Stop any
web server already holding 80 or 443 first.

On Docker, drop the `caddy` service and publish npanel's ports instead. In
the `configs` block, set `port = 80` and `https_port = 443` in place of `port`
and `tls_check_port`. Docker lets a container's own user bind low ports, so
nothing else is needed but a health check that fits — with no proxy there is no
check port, and plain HTTP only redirects:

```yaml
services:
  npanel:
    ports:
      - "80:80"
      - "443:443"
    healthcheck:
      test:
        - CMD-SHELL
        - >-
          wget -S -q -O /dev/null --max-redirect=0 --header "host: ${DASHBOARD_HOST}"
          http://127.0.0.1:80/ 2>&1 | grep -q " 308 "
```

Keep the published ports the same as the container's: the redirect to HTTPS
names `https_port`, so `443:8443` would send visitors to `:8443`.

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
proxy_http_version 1.1;
proxy_set_header Upgrade $http_upgrade;
proxy_set_header Connection $connection_upgrade;
```

The last three pass on an upgraded connection, which a [tunnel](#tunnels)'s
agent connects with and its WebSockets are. `$connection_upgrade` needs the
usual `map $http_upgrade $connection_upgrade { default upgrade; '' close; }`
in the `http` block. Caddy does all of this by itself.

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
the internet. It listens on `127.0.0.1` unless you change `bind_address`; if
the proxy is on another machine, set `bind_address = "0.0.0.0"` and firewall the
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
its guards — goes in `data_dir/tor`. It is not
worth backing up on its own: losing it costs a slower start and no addresses.

Tor checks the permissions of the directories it writes to, and refuses to
start in one that another user could change: `data_dir/tor`, and every
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
are always there, except those about onion services, which are each one
failure Tor retries on its own — a directory that wouldn't take a descriptor, a
visitor's circuit that didn't complete — and come by the hundred an hour while
every site stays reachable. What matters about a service is whether it can be
reached, so npanel says that itself: `tor_onion_unreachable`, a warning with
Tor's account of the problem, once a site has been unreachable for ten
minutes, and `tor_onion_reachable` when it is back. Set `log_level = "debug"`
to see everything else Tor is doing, each service's state changes as
`tor_onion_state` included.

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

## Tunnels

A tunnel serves a domain from a web server on another machine: a laptop, a
box on a home network, anything that can reach your npanel server. Nothing on
that machine needs a public address or an open port. Run this there, with the
port the web server listens on:

```sh
npanel tunnel npanel.example.com 8080
```

The first time, it prints a link to your dashboard and a code. Open the link
as an admin, check that it shows the same code, and give the tunnel a domain
— one no site or tunnel here has yet, with its DNS pointed at npanel as for a
site. The agent connects as soon as you approve it, and the domain serves
whatever `localhost:8080` does, WebSockets included.

The target can be a port, a host and port (`devbox:5173`), or an
`http://` address; the agent forwards plain HTTP. It sends the origin its own
address as `Host`, which development servers accept, and the public domain as
`X-Forwarded-Host`; add `--preserve-host` to send the public domain as `Host`
instead. `--name` sets what the dashboard calls it, which is otherwise the
machine's name.

The agent holds two connections to the server and uses every core. For a busy
origin, such as a Blossom server, give it more with `--connections 8`, or run
the same command on several machines with the same key file: they share the
tunnel, and npanel sends requests to each in turn. All of a tunnel's agents
together may hold `tunnel_max_connections`, 100 unless an admin changes it
under **Settings → Tunnels**.

The agent keeps its key under `~/.config/npanel/tunnels/`, one per server and
target. Running the same command again is the same tunnel, with no approval,
and a different port is a different tunnel. Keep it running however you keep
anything running: a terminal, `tmux`, or a systemd user unit. It reconnects
by itself when either end restarts.

**Tunnels** in the dashboard shows each tunnel's connections, what it forwards
to, and its certificate. Turning one off frees its domain at once, and the
agent waits to be turned back on. Removing one disconnects it for good: its
key works nowhere until it is approved again.

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

Everything is in `data_dir`. The blob store is content-addressed and re-fetchable
from Blossom servers, so the database is the part that actually matters — losing
blobs costs a re-download, losing the database costs your configuration.

Back up SQLite with the online backup API rather than copying the file, which
can catch a write in progress:

```sh
sqlite3 /var/lib/npanel/npanel.sqlite3 ".backup '/backup/npanel-$(date +%F).sqlite3'"
```

On Docker the same call runs in a throwaway container against the volume — see
[The volumes](#the-volumes).

To back up blobs too, `rsync` `data_dir/blobs` — files there are immutable and
named by hash, so incremental syncs are cheap. `previews.sqlite3` is a cache of
what relays said, and needn't be backed up at all.

## Upgrading

Install the new release's binary over the old one and restart:

```sh
sudo install -m 755 npanel-0.6.2-linux-x86_64 /usr/local/bin/npanel
sudo systemctl restart npanel
```

From a package, install the new release's package the same way as the first;
it restarts npanel itself.

From source, `git pull`, rebuild the dashboard and the binary as in
[section 2](#from-source), and install it the same way.

Migrations run automatically at startup. Back up the database first. On Docker
this is [`docker compose up -d --build`](#upgrading-the-containers) instead.

### From `.env` to `npanel.toml`

Up to 0.5, npanel read its configuration from `.env` in its working directory.
It now reads only `npanel.toml` and refuses to start without one, saying so if
it sees a `.env` beside it. Move each setting across with its name in lowercase,
numbers unquoted:

```
DASHBOARD_HOST="panel.example.com"      dashboard_host = "panel.example.com"
DATA_DIR="/var/lib/npanel"         →    data_dir = "/var/lib/npanel"
HTTPS_PORT="443"                        https_port = 443
```

Write the result to `/etc/npanel/npanel.toml`, install the new unit from
[section 4](#4-systemd), which names that file instead of setting a working
directory, and delete the `.env`:

```sh
sudo install -m 644 npanel.service /etc/systemd/system/npanel.service
sudo systemctl daemon-reload
sudo systemctl restart npanel
sudo rm /etc/npanel/.env
```

On Docker, put `DASHBOARD_HOST` in compose's `.env` as before; `compose.yaml`
writes npanel's config from it. Any other variable you set for npanel goes in
its `configs` block instead.

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
no `dashboard/dist`: a development build. Run `bun install && bun run build`
in `dashboard/`. The server logs `dashboard_not_built` once with the
directories it looked in. A server should run a binary from
`contrib/release/build.sh`, as releases and the image do, which can't say
this.

**A site returns 503 "This site has not finished deploying yet."** No version is
ready. Check the version list and its deploy log in the dashboard.

**Login fails with "Auth event was signed for … not …".** `dashboard_host`
doesn't match the hostname in the browser. It must be the public name, not the
proxy's upstream address.

**Login fails with "Auth event expired."** The clock on the server or the client
is off by more than a minute. Run NTP.

**Login fails with "This account has no access to npanel."** The signature is
valid but the pubkey isn't a user. Add it from the Users page, or bootstrap with
`make-admin`.

**A site has no certificate, with `https_port` set.** Its page in the dashboard
says why, in the certificate authority's words, and the log has it as
`tls_certificate_failed`. Usually DNS doesn't point here yet, or a firewall
blocks port 80 or 443. npanel keeps trying on its own. A browser connecting
before there is one gets a TLS error rather than someone else's certificate.

**Caddy won't issue a certificate.** Confirm DNS resolves to the box, the host
exists in the dashboard and is enabled, and that
`curl 'http://127.0.0.1:3001/tls-check?domain=example.org'` returns `ok`. A
`Caddyfile` still pointing `ask` at `:3000/api/tls-check` gets a `404` for every
name and denies them all — that endpoint moved to its own port.
