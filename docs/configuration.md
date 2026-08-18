# Configuration

npanel splits configuration in two.

**Environment variables** cover things the process needs before it can talk to a
database, plus anything that could lock an admin out of the dashboard if it were
set wrong. There are seven of them, and they require a restart.

**Runtime settings** cover everything else. They live in the `settings` table,
are edited by admins on the dashboard's Settings page, take effect immediately
without a restart, and are recorded in the audit log.

## Environment variables

Copy `.env.example` to `.env` and edit it. Bun loads `.env` automatically.

On Docker only `DASHBOARD_HOST`, `SECRET_KEY` and `LOG_LEVEL` are read from that
file, by compose, which passes them to the containers; the image fixes the other
four. See [deployment](deployment.md#docker-compose).

### `PORT`

Default `3000`. The port the HTTP server binds. Your reverse proxy forwards
here. Must be 1–65535 or startup fails.

### `BIND_ADDRESS`

Default `127.0.0.1`. The address the HTTP server listens on. Must be an IP
address, not a hostname, or startup fails.

Loopback by default because npanel is only ever meant to be reachable by the
reverse proxy: it trusts `X-Forwarded-Host` from whoever connects to it, and
`/api/tls-check` answers without authentication on any hostname. Exposed
directly, either of those is a hole.

Set `0.0.0.0` when the proxy runs on another machine or in another container,
and firewall the port to it.

Both listeners bind here: the public one on `PORT` and certificate
authorization on [`TLS_CHECK_PORT`](#tls_check_port).

The Docker image sets `0.0.0.0` for the reason above — a container's loopback is
reachable by nothing else, not even the proxy beside it — and `compose.yaml`
takes the place of the firewall by never publishing either port.

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

### `DASHBOARD_HOST`

Required, with no default. The hostname that serves the dashboard and the JSON
API — for example `panel.example.com`. Give it a bare hostname: no scheme, no
path. A trailing `:port` is stripped.

This one value does a lot of work:

- Requests arriving with this `Host` header get the dashboard and `/api`.
  Requests with any other hostname are served as nsites.
- NIP-98 auth events are checked against it, so a signature made for a different
  hostname is rejected.
- `/api/tls-check` approves it for a certificate.

Because it's compared to the *public* hostname, it must be the name users type
in the browser, not the address your proxy forwards to.

### `SECRET_KEY`

No default, and optional. 64 hex characters — 32 bytes — encrypting the signer
credentials the [bunker gateway](architecture.md#bunkers) holds. Generate one
with:

```sh
openssl rand -hex 32
```

Unset, bunkers are unavailable: the section reports that this server has nowhere
safe to keep credentials, and every other part of npanel works exactly as
before. Nothing else reads this value, so an existing install upgrades without
touching it.

It is an environment variable rather than a runtime setting for the obvious
reason: a key stored beside the rows it protects is not protecting them. Keep it
out of the database backup, and out of the same place you keep `DATA_DIR`.

**Changing it strands every bunker.** The credentials already stored cannot be
read back with a different key; npanel notices — each bunker records which key
sealed it — and says so rather than failing later as a decryption error, but the
only fix is to pair each bunker again. Treat it as permanent.

What it protects is a stolen database, a leaked backup, a discarded disk. What
it does not protect is a compromise of the running server, which has the key in
its environment by construction. That is the honest boundary, and it is still
worth having: it is the difference between a database dump being enough and
needing the machine as well.

### `LOG_LEVEL`

Default `info`. One of `debug`, `info`, `warn`, `error`. Logs are one flat JSON
object per line on stdout, which `journalctl -o cat | jq` reads nicely. `debug`
adds per-request lines for served sites and is loud on a busy gateway.

### `DATA_DIR`

Default `./data`, resolved to an absolute path. Contains:

- `npanel.sqlite3` — hosts, users, versions, settings, audit log
- `blobs/` — the content-addressed file store

Both are created on first run. This directory is the entire state of the
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

At least one relay is required, and each must be a `ws://` or `wss://` URL.
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

It applies when a host is created and when one is renamed. Hosts that already
exist keep working, so tightening it never takes a site offline.

### `tls_check_limit_per_host`

Default `60`, allowed range 1–10000. How many certificates `/api/tls-check` will
authorize for a single hostname in an hour.

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

### `bunker_relays`

Default empty, which means use [`relays`](#relays). The relays the bunker
gateway listens on and advertises in the connection URIs it mints. At most
eight, each a `ws://` or `wss://` URL.

Worth setting separately. NIP-46 is ephemeral request/response chatter that has
nothing to do with finding site manifests, and pointing it at one relay both
bounds how many sockets the gateway holds open and keeps signing traffic off
relays chosen for something else. A relay that drops ephemeral events, or that
rate limits, will make signing unreliable in a way that looks like the phone not
answering — prefer one you run.

Only connections minted after a change use the new list; existing ones keep the
relays they were minted with, so their clients don't stop being able to reach
npanel because an admin edited a setting.

### `bunker_request_timeout`

Default `20` seconds, allowed range 5–120. How long the gateway waits for an
upstream signer to answer before giving up on one request.

The wait is real: the answer comes from an app on someone's phone, which may be
asleep, offline, or waiting for a tap. Too short and a signer that would have
answered gets reported as broken; too long and a client hangs instead of being
told something is wrong.

### `bunker_max_relays`

Default `8`, allowed range 1–32. How many relays the gateway will hold open at
once, across every connection.

A client connecting with `nostrconnect://` names its own relays, and honouring
all of them without limit is how a gateway ends up holding a socket to
everywhere. Minting a connection that would push past this is refused, with a
message saying so — the budget is spent where somebody is present to be told,
rather than quietly.

The gateway is frugal with what it does hold: subscriptions are keyed by relay,
not by connection, so a hundred connections reachable on one relay cost one
socket and one filter between them. No connection may pull in more than four
relays of its own regardless of this setting.

### `bunker_rate_limit`

Default `60`, allowed range 1–6000. Requests per minute one connection may make
before being refused.

It is per connection rather than per bunker, so a runaway script holding one
credential can't starve the others. A refusal is an error the client is sent,
not silence. A connection that legitimately signs in bursts — a bulk publish, a
migration — wants this raised.

## Precedence and validation

Runtime settings are read from the database at startup and cached in memory. A
row that fails validation — say, a relay URL that was valid under an older
version — is logged as `setting_invalid` and the default is used instead, so a
bad row can never stop the server from booting. Missing rows are seeded with
defaults on first run.
