# Configuration

npanel splits configuration in two.

**Environment variables** cover things the process needs before it can talk to a
database, plus anything that could lock an admin out of the dashboard if it were
set wrong. There are five of them, and they require a restart.

**Runtime settings** cover everything else. They live in the `settings` table,
are edited by admins on the dashboard's Settings page, take effect immediately
without a restart, and are recorded in the audit log.

## Environment variables

Copy `.env.example` to `.env` and edit it. Bun loads `.env` automatically.

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

### `resync_interval`

Default `600` seconds, allowed range 60–86400. How often npanel re-fetches every
configured site's newest manifest from relays.

The live subscription normally catches new deploys within seconds; this is the
backstop for manifests missed during relay downtime or a restart. Lowering it
makes npanel notice sooner at the cost of more relay traffic. Changing it
re-arms the timer immediately.

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

## Precedence and validation

Runtime settings are read from the database at startup and cached in memory. A
row that fails validation — say, a relay URL that was valid under an older
version — is logged as `setting_invalid` and the default is used instead, so a
bad row can never stop the server from booting. Missing rows are seeded with
defaults on first run.
