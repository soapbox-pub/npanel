# Configuration

npanel splits configuration in two.

**Environment variables** cover things the process needs before it can talk to a
database, plus anything that could lock an admin out of the dashboard if it were
set wrong. There are seven of them, and they require a restart.

**Runtime settings** cover everything else. They live in the `settings` table,
are edited by admins on the dashboard's Settings page, take effect immediately
without a restart, and are recorded in the audit log.

## Environment variables

Copy `.env.example` to `.env` and edit it. npanel loads `.env` from its working
directory at startup, without overriding anything already set in the environment.

On Docker only `DASHBOARD_HOST` and `LOG_LEVEL` are read from that file, by
compose, which passes them to the containers; the image fixes the rest.
See [deployment](deployment.md#docker-compose).

### `PORT`

Default `3000`. The port the HTTP server binds. Your reverse proxy forwards
here. Must be 1–65535 or startup fails.

With [`HTTPS_PORT`](#https_port) set there is no proxy, and this is plain HTTP
facing the internet — `80`, since that is where a certificate authority's
HTTP-01 challenge connects. It then answers those challenges and redirects
everything else to HTTPS, and serves nothing in the clear.

### `BIND_ADDRESS`

Default `127.0.0.1`. The address the HTTP server listens on. Must be an IP
address, not a hostname, or startup fails.

Loopback by default because npanel is only ever meant to be reachable by the
reverse proxy: it speaks plain HTTP with no certificate of its own, and
certificate authorization on [`TLS_CHECK_PORT`](#tls_check_port) answers
without authentication. Exposed directly, either of those is a hole.

Set `0.0.0.0` when the proxy runs on another machine or in another container,
and firewall the port to it.

Both listeners bind here: the public one on `PORT`, and certificate
authorization on [`TLS_CHECK_PORT`](#tls_check_port).

The Docker image sets `0.0.0.0` for the reason above — a container's loopback is
reachable by nothing else, not even the proxy beside it — and `compose.yaml`
takes the place of the firewall by never publishing either port.

With [`HTTPS_PORT`](#https_port) set the default is `::` instead, every address,
IPv6 and IPv4 alike: there is no proxy, and visitors connect directly. On a host
with IPv6 turned off entirely, set `0.0.0.0`.

### `TLS_CHECK_PORT`

Default `3001`. The port that answers your reverse proxy's certificate
authorization requests — Caddy's `on_demand_tls ask` — on the same address as
`PORT`. Must be a valid port, and must differ from `PORT`, or startup fails
rather than leaving you to decode an `EADDRINUSE`.

It is a second listener because the request has nowhere to say which hostname it
is about: Caddy makes a plain `GET` to the address it proxies to, so npanel
cannot route it by `Host` the way it routes everything else. On the public port
the endpoint would therefore have to answer on *every* hostname npanel serves,
which puts it on the internet — and anyone could then drive approvals for a
configured host until the hourly budget below was spent, denying certificates to
the sites that need them.

Nothing else is served there, and it must be reachable only by the proxy. See
[deployment](deployment.md#5-tls-with-caddy).

Not bound with [`HTTPS_PORT`](#https_port) set: there is no proxy to ask.

### `HTTPS_PORT`

Unset by default, and a reverse proxy terminates TLS. Set it — to `443` — and
npanel serves HTTPS itself on that port, getting a certificate for every name
it serves from an ACME certificate authority, Let's Encrypt unless
[`acme_directory`](#acme_directory) says otherwise, and renewing each before it
expires. [`PORT`](#port) becomes the plain-HTTP side, which must be `80`. Must
differ from `PORT`, or startup fails.

Setting it agrees to the certificate authority's terms of service. See
[deployment](deployment.md#without-a-proxy).

The port is named in the redirects plain HTTP sends, so if anything forwards
ports to npanel, forward them unchanged.
### `DASHBOARD_HOST`

Required, with no default. The hostname that serves the dashboard and the JSON
API — for example `panel.example.com`. Give it a bare hostname: no scheme, no
path. A trailing `:port` is stripped.

This one value does a lot of work:

- Requests arriving with this `Host` header get the dashboard and `/api`.
  Requests with any other hostname are served as nsites.
- NIP-98 auth events are checked against it, so a signature made for a different
  hostname is rejected.
- It is always approved for a certificate — by `/tls-check`, or by npanel's own
  ACME client with `HTTPS_PORT` — and never counts against the limits below.

Because it's compared to the *public* hostname, it must be the name users type
in the browser, not the address your proxy forwards to.

### `LOG_LEVEL`

Default `info`. One of `debug`, `info`, `warn`, `error`. Logs are one flat JSON
object per line on stdout, which `journalctl -o cat | jq` reads nicely. `debug`
adds per-request lines for served sites and is loud on a busy gateway.

### `DATA_DIR`

Default `./data`, resolved to an absolute path. Contains:

- `npanel.sqlite3` — hosts, users, versions, settings, audit log
- `blobs/` — the content-addressed file store
- `analytics.sqlite3` — visits and pageviews, for the sites that count them
- `geo/` — the location database, downloaded once a site counts visits
- `tor/` — Tor's directory cache and state, once Tor is turned on
- `previews.sqlite3` and `preview-images/` — link previews sites' scripts made,
  and the images they drew, a cache that can be deleted at any time

Each is created when first needed. This directory is the entire state of the
system; see [deployment](deployment.md#backups) for backups.

## Runtime settings

Admins edit these at **Settings** in the dashboard, or through
`GET`/`PUT /api/settings`. Each key is validated on write, and a bad value is
rejected with a readable error rather than being stored.

### `relays`

Default:

```
wss://relay.ditto.pub
wss://relay.dreamith.to
wss://relay.primal.net
```

The relays npanel uses for everything: watching for new site manifests, and
looking up authors' NIP-65 relay lists (kind 10002) and Blossom server lists
(kind 10063). There is no separate lookup-relay list.

At least one relay is required, and each must be a `wss://` URL. Plain `ws://`
is refused here and everywhere else npanel takes a relay: an unencrypted
connection lets anything on the path read or rewrite what the relay says.
Consider adding a couple of profile-oriented relays (`purplepag.es`,
`user.kindpag.es`) — that's where kind 10002 and 10063 events usually live, and
without them npanel may not learn where an author's blobs are.

Changing this reconnects the pool, clears the relay-list caches, and restarts
the live subscription. Nothing needs a restart.

### `blossom_fallback_servers`

Default:

```
https://blossom.ditto.pub/
https://blossom.dreamith.to/
https://blossom.primal.net/
```

Blossom servers to try when a manifest carries no `server` tags *and* its author
publishes no kind 10063 list. Most sites need none of these, because publishing
tools include server hints.

If a deploy fails with "No Blossom servers to download from", this is the fix.
Adding servers here immediately retries every version that isn't ready.

Values must be `http://` or `https://` URLs. A trailing slash is added so a path
prefix is preserved when the blob URL is built.

### `max_file_size`

Default `104857600` (100 MiB), maximum 8 GiB. The largest single blob npanel
will download. A blob that exceeds it is aborted mid-stream and the version
fails with the reason recorded in its deploy log. Raising the limit retries
incomplete versions.

### `max_site_size`

Default `1073741824` (1 GiB), allowed range 1 MiB–1 TiB. The largest one version
of a site may be: every blob it references added up, including ones another
site already brought. What a deploy downloads is decided by the site's author,
not by anyone with access to npanel, so this is what keeps one author from
filling the disk.

A manifest doesn't say how big its files are, so npanel finds out by
downloading. Once a version passes the limit nothing new is started, the few
downloads already under way finish, and the version fails. It is not retried,
since the next attempt would meet the same answer; raising the limit retries
incomplete versions.

### `max_site_files`

Default `50000`, allowed range 1–1,000,000. The most files one version of a site
may list. A version over it fails before anything is downloaded, and, like
`max_site_size`, raising the limit retries it.

Only a site's newest version is ever downloaded. A manifest that arrives after a
newer one from the same site — or that is still waiting its turn when a newer
one lands — fails as superseded rather than being fetched in full.

### `deploy_attempts`

Default `3`, allowed range 1–10. How many times a version is deployed before
npanel stops trying for now. `1` never retries.

A retry resumes rather than restarts: blobs downloaded by an earlier attempt are
already on disk, so only what is still missing is fetched again. The version
stays `failed` in between, and its host goes on serving whatever version it
already had.

Attempts are spaced out — five seconds, then ten, doubling each time — because a
deploy usually fails over something that is unavailable rather than something
that is momentarily busy. A version sitting out its wait holds up nothing: other
versions deploy while it waits.

Attempts are counted per run, not for the lifetime of a version. Once a run is
spent, `resync_interval` still brings the version back — which is what lets a
site recover on its own when a Blossom server holding its blobs comes back hours
later.

### `resync_interval`

Default `600` seconds, allowed range 60–86400. How often npanel re-fetches every
configured site's newest manifest from relays.

The live subscription normally catches new deploys within seconds; this is the
backstop for manifests missed during relay downtime or a restart. Lowering it
makes npanel notice sooner at the cost of more relay traffic. Changing it
re-arms the timer immediately.

### `keep_versions`

Default `20`, allowed range 0–1000. How many ready versions of each site npanel
keeps; `0` keeps every one. Once a day, and as soon as this changes, npanel
forgets the ready versions past the newest `keep_versions` of each site, and
deletes every stored file no remaining version or build uses.

Only ready versions count, because they are the ones a site can be rolled back
to. A version that never finished deploying and is older than the newest ready
one is forgotten whatever this says: only a site's newest version is ever
deployed, so it never will be. The version a site is serving and the one it is
pinned to are always kept, as is anything newer than the newest ready version,
which may still be deploying.

Versions of a site no host points at are forgotten whatever this says, `0`
included. A disabled host still points at its site. Adding a host for the site
again fetches its newest version from relays.

On a host that merges deployments, this also bounds how far back the merge
reaches. See [pruning](architecture.md#pruning).

### `dns_targets`

Default empty. What the dashboard tells staff to point a domain at, shown in
the *Add site* dialog. Each value becomes one record:

| Value                 | Record                        |
| --------------------- | ----------------------------- |
| `203.0.113.10`        | `A 203.0.113.10`              |
| `2001:db8::1`         | `AAAA 2001:db8::1`            |
| `gateway.example.com` | `CNAME gateway.example.com`   |

A hostname must be the only entry, because DNS doesn't allow a CNAME beside any
other record for the same name. Up to eight values otherwise.

Left empty — the default — npanel resolves `DASHBOARD_HOST` and repeats its A
and AAAA records. Nobody reaches the dashboard without having pointed that name
at this server already, so the answer is right without anyone configuring it.
The result is cached for five minutes.

Set this when that inference is wrong or unhelpful: the dashboard sits behind a
CDN whose addresses aren't where nsites should go, the machine has more
addresses than the dashboard hostname advertises, or you want staff to use a
CNAME so the address can change without touching every zone.

Nothing here affects routing — npanel serves whatever arrives with a configured
`Host` header regardless. It is advice for the human editing the zone, so a
wrong value costs a confusing DNS record, not an outage.

### `allowed_host_suffixes`

Default empty, which allows any hostname. Otherwise a hostname must equal one of
these domains or sit under it — `example.com` permits `example.com` and
`blog.example.com`, but not `notexample.com`.

Adding a host authorizes a certificate for it, and any staff user can add one.
Certificate authorities meter issuance per registered domain — Let's Encrypt
allows roughly 50 certificates per week per eTLD+1 — so without a restriction
here, one enthusiastic or compromised account can exhaust issuance for a domain
that everything else on the gateway shares. Setting this to the domains you
actually serve bounds that.

It is also the only thing bounding *which* names staff can claim. A grant covers
the hosts it names, but adding a host needs no grant: any staff user can take
any hostname nobody has configured yet, including the `www` of a root domain
someone else serves — a `www` host configured deliberately wins over that
domain's redirect — or a subdomain under it. npanel doesn't check that whoever
adds a name controls its DNS. On a gateway shared by teams that don't trust one
another, list the domains each may use here, or keep the staff role to people
you would trust with all of them.

It applies when a host is created and when one is renamed. Hosts that already
exist keep working, so tightening it never takes a site offline.

### `tls_check_limit_per_host`

Default `60`, allowed range 1–10000. How many certificates `/tls-check` will
authorize for a single hostname in an hour. With [`HTTPS_PORT`](#https_port) it
is how many certificates npanel orders for one hostname in an hour instead.

Caddy asks before every TLS handshake for a name it holds no certificate for, so
a normal host costs one approval and then goes quiet. A host whose DNS never
points at this machine is the problem case: validation fails, no certificate is
cached, and the next handshake asks again — forever. Anyone can drive that loop
by opening TLS connections with that name as SNI, no DNS required, so the budget
drains without a bound. This is that bound.

The default leaves plenty of room for a cold start, where many first visitors
can ask at once before the certificate is in place. Lower it if you are close to
a certificate authority's limits; raise it if a busy new host ever fails to get
a certificate.

### `tls_check_limit_total`

Default `600`, allowed range 1–100000. The same, across every hostname at once.
This is the backstop for many hosts failing together, or for someone adding
hundreds of them.

Only approvals count against either limit. Asking about a hostname nobody
configured is refused with `404` and costs nothing — metering those instead
would let a stranger spraying names starve certificates for the hosts that are
configured, trading a slow drain for an easy outage. The dashboard hostname is
exempt for the same reason: an exhausted budget must not be able to lock admins
out of npanel itself.

Throttled asks are logged as `tls_check_throttled`.

With [`HTTPS_PORT`](#https_port), the same two limits meter npanel's own orders,
logged as `tls_order_throttled` when one is held back. They matter less there:
npanel waits longer after each failed attempt for a name — two minutes, then
four, eight, and so on to an hour — so a name whose DNS points elsewhere costs
five attempts in its first hour and one an hour after that, within Let's
Encrypt's limit of five failed validations per name an hour. Let's Encrypt also
caps new orders at 300 per three hours for an account, which `600` an hour does
not respect: lower `tls_check_limit_total` to `100` on a gateway with many
names failing at once.

### `acme_directory`

Default `https://acme-v02.api.letsencrypt.org/directory`, Let's Encrypt. The
ACME directory npanel gets certificates from with [`HTTPS_PORT`](#https_port);
ignored behind a proxy. Must be an `https://` URL.

Point it at Let's Encrypt's staging CA,
`https://acme-staging-v02.api.letsencrypt.org/directory`, to try a new setup
without spending the real one's limits. Its certificates aren't trusted by
browsers. The Settings page has a switch for exactly that.

Certificates and accounts are kept per directory. Switching serves the new
one's certificates only, getting each name one as it goes, and switching back
finds the old ones still there if they haven't expired.

### `acme_email`

Default empty. Where the certificate authority writes about problems with the
certificates — Let's Encrypt no longer sends expiry warnings, but does write
about incidents that affect them. Changing it updates the account.
### `contact_npub`

Default empty. Whoever runs this instance, shown on the logged-out homepage
alongside a link to their profile.

npanel is meant to be self-hosted, so a stranger who follows a link to a site
you serve and then wonders what npanel is arrives at a gateway that isn't
theirs. Naming an operator is what turns that page from an advertisement for
some anonymous service into "this is *someone's* instance, and you can run your
own". Left empty, the homepage just says nobody is listed.

Paste an npub, an nprofile, or a hex pubkey; all three are stored as an npub.
It is read by `GET /api/instance`, which is unauthenticated — this value is
published to be shown in public, so don't set it to a key you want kept quiet.

### `public_site_list`

Default `false`. Whether npanel will name the sites it serves to people who
can't manage them: a list on the logged-out homepage, and a *Also on this
gateway* panel for staff, showing the sites they have no grant for.

Each listed site is shown as a link preview — the title, blurb, and image its
own landing page carries — read from the copy npanel already has on disk rather
than by crawling anything. A site that publishes no metadata is listed as a bare
name.

Off by default because which sites a gateway serves is often the private part of
a private deployment — a team's staging domains, a client's site before launch.
Turning it on publishes nothing but the domain names themselves, which DNS
already tells anyone who asks; it does not reveal which nsite a site points at,
or who added it.

Disabled sites are never listed, since they answer `404` and listing one would
only advertise a dead link. Individual sites opt out with their **unlisted**
flag, set when adding a site or from its page afterwards — that flag wins over
this setting, and is the way to keep one domain quiet on a gateway that
otherwise publishes its list. Neither this setting nor the flag affects who can
*visit* a site; a served site is public either way.

### `tor_enabled`

Default `false`. Whether every enabled site also gets a Tor onion address of its
own.

Turning it on is the whole of the setup. Tor is built into npanel, so there is
no daemon to install: npanel joins the Tor network, starts one onion service per
site, and keeps the key each one is named by. There is nothing to configure per
site and nothing for staff to do — a site added afterwards has an address within
a second or two of being created, and a site removed loses its address with it.
The first time, joining the network takes a minute or so before the addresses
answer; the Settings page shows how far along it is.

The addresses are stable, because npanel stores the keys in its database. Tor
keeps them only in memory, which is what stops an onion from answering at an
address whose server is gone. What Tor writes to disk lives under
`DATA_DIR/tor`, and losing it costs a slower start and no address.

Off by default because an upgrade shouldn't start publishing a gateway's sites
somewhere their admin never asked for. See [deployment](deployment.md#tor).

Sites cannot opt out individually. An onion address is a second door to files
that are already public on the first one, and it is the gateway operator's
decision whether this gateway has one.

### `analytics_timezone`

Default `UTC`. The IANA time zone every site's analytics are reported in, like
`Europe/Berlin` — where one day's visitors end and the next day's begin, for
the charts and every date range. Validated against the IANA database compiled
into npanel, which is what does the arithmetic, matched case-insensitively and
stored in its canonical spelling.

It is one clock for the whole instance rather than one per site: a gateway is
usually run by one team, reading all of its sites' numbers together. Changing it
re-draws everything on the new clock, past days included, because events are
stored with their exact time and bucketed only when a chart asks. Whether a site
is counted at all is its own [`analytics`](api.md#patch-apihostshostname) flag.

### `link_preview_crawler_pattern`

Which User-Agents get a [link preview](architecture.md#link-previews): a
regular expression, matched case-insensitively against the first 512
characters of the header. The default names the search engines, and the
services that unfurl a link pasted into a chat or a post — Google, Bing,
DuckDuckGo, Apple, X, Discord, Facebook, LinkedIn, Slack, Telegram, WhatsApp,
Mastodon and the other fediverse servers, Bluesky, and a few more. Everyone it
doesn't match gets the page as published.

It can't be empty: a pattern that matches nothing would be a way to turn
previews off, which is a site's switch, not this one. It is compiled with a
size limit, so a pattern that would be expensive to match is refused when it is
saved.

### `link_preview_ttl`

Default `3600`. Seconds a preview is served before its site's script runs for
it again. A stale preview is still served while the new one is made behind it,
so this decides how soon an edited post shows, not how often a crawler waits.
A script having nothing for a page is believed for ten minutes at most, since
it may only mean the relays were slow.

### `link_preview_concurrency`

Default `256`. How many lookups may run at once. Past it, a crawler that asks
for a preview nobody has cached gets the page as published straight away rather
than waiting in line. Lookups run on a thread of their own, and never on the
ones serving sites; the scripts themselves run in as many processes at once as
there are cores, and the rest wait their turn.

### `link_preview_misses_per_minute`

Default `600`. How many times a minute sites' preview scripts may run.
Previews already cached don't count, stale ones included, so this is the most
that someone requesting made-up event ids can make the scripts ask relays and
fetch for — which matters because those are the relays npanel's deploys
depend on too. No one site may spend more than half of them. A lookup over it
is answered from what is cached, if anything.

### `link_preview_max_entries`

Default `100000`. How many previews are kept on disk, in
`DATA_DIR/previews.sqlite3`. Past it, the least recently made go first. A
preview is a few kilobytes, one with an article up to a few hundred.

### `errors_retention_days`

Default `90`, between 1 and 365. How long error reports are kept, in
`DATA_DIR/errors.sqlite3`: an older report is forgotten, and so is an issue
with none newer — along with its counts. Checked hourly.

## Not a setting: the Content Security Policy

Served sites carry a `Content-Security-Policy`, and there is no instance-wide
switch for it — deliberately. See [the `csp` flag](api.md#patch-apihostshostname),
which is per site.

A gateway serves other people's code under domains you own, so the blast radius
of one careless deploy is one hostname, not all of them. A global off switch
would turn that into a single tick-box that disarms every site at once, which is
the one mistake a security default shouldn't make easy. The site that needs
inline scripts turns the policy off for itself and everything else stays
protected.

Nor is the policy itself a setting. A site that declares one in the `<meta>`
tags of its `/index.html` is served under that one; a site that declares none
gets a restrictive default. Neither is an admin's to write, and there is nothing
to configure here — the site decides, per deploy, and the host page in the
dashboard shows which policy each one ended up with.

## Precedence and validation

Runtime settings are read from the database at startup and cached in memory. A
row that fails validation — say, a relay URL that was valid under an older
version — is logged as `setting_invalid` and the default is used instead, so a
bad row can never stop the server from booting. Missing rows are seeded with
defaults on first run.
