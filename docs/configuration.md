# Configuration

npanel splits configuration in two.

**Environment variables** cover things the process needs before it can talk to a
database, plus anything that could lock an admin out of the dashboard if it were
set wrong. There are four of them, and they require a restart.

**Runtime settings** cover everything else. They live in the `settings` table,
are edited by admins on the dashboard's Settings page, take effect immediately
without a restart, and are recorded in the audit log.

## Environment variables

Copy `.env.example` to `.env` and edit it. Bun loads `.env` automatically.

### `PORT`

Default `3000`. The port the HTTP server binds. Your reverse proxy forwards
here. Must be 1–65535 or startup fails.

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
wss://relay.damus.io
wss://nos.lol
wss://relay.nostr.band
wss://purplepag.es
wss://user.kindpag.es
```

The relays npanel uses for everything: watching for new site manifests, and
looking up authors' NIP-65 relay lists (kind 10002) and Blossom server lists
(kind 10063). There is no separate lookup-relay list.

At least one relay is required, and each must be a `ws://` or `wss://` URL.
Include a couple of profile-oriented relays (`purplepag.es`,
`user.kindpag.es`) — that's where kind 10002 and 10063 events usually live, and
without them npanel may not learn where an author's blobs are.

Changing this reconnects the pool, clears the relay-list caches, and restarts
the live subscription. Nothing needs a restart.

### `blossom_fallback_servers`

Default empty. Blossom servers to try when a manifest carries no `server` tags
*and* its author publishes no kind 10063 list. Most sites need none of these,
because publishing tools include server hints.

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

### `allow_private_blob_servers`

Default `false`. Whether npanel may download blobs from loopback, LAN, and other
non-public addresses.

Download targets are not npanel's to choose: they come from the manifest's own
`server` tags, from the author's kind 10063 list, and from whatever those
servers redirect to. Left unrestricted, publishing a manifest is enough to make
npanel issue GET requests to anything reachable from the machine it runs on —
cloud metadata, an admin interface on the LAN, a database's HTTP port — and the
resulting status codes land in a deploy log the author's operators can read.
The response body can't leak, since it has to hash to the requested blob or it
is discarded, but the probe itself is the problem.

So each hop is resolved and refused unless every address it answers with is
public. Turn this on if your Blossom server is on your own network; the check
is then skipped entirely, for every source. Note that the check resolves the
name rather than pinning the connection to the address it checked, so a
determined DNS-rebinding attack can still get through: don't run npanel
somewhere a single GET request to a neighbouring service would be a disaster.

Changing this retries every version that isn't ready.

## Precedence and validation

Runtime settings are read from the database at startup and cached in memory. A
row that fails validation — say, a relay URL that was valid under an older
version — is logged as `setting_invalid` and the default is used instead, so a
bad row can never stop the server from booting. Missing rows are seeded with
defaults on first run.
