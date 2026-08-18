# Architecture

npanel is one process that does three things: it serves nsites over HTTP, it
serves a dashboard for configuring which nsites those are, and it runs a
background worker that keeps local storage in sync with Nostr.

A fourth thing, the [bunker gateway](#bunkers), runs in a worker thread beside
them, and only when a `SECRET_KEY` is set.

```
        DNS ──► reverse proxy (TLS) ──► npanel :3000
                                          │
                    ┌─────────────────────┴────────────────────┐
                    │  Host = DASHBOARD_HOST?                  │
                    ├─────────────────────┬────────────────────┤
                    │ yes                 │ no                 │
                    ▼                     ▼                    │
              /api  + dashboard SPA   nsite from local store   │
                    │                     ▲                    │
                    │                     │                    │
                    ▼                     │                    │
                 SQLite ◄──── sync worker ┘                    │
                    ▲             │                            │
                    │ relays (manifests) ── Blossom (blobs) ───┘
                    │
                    └─ npanel :3001 ◄── proxy: may this name have a certificate?
```

## Request routing

Everything hangs off the `Host` header, checked in one middleware in
`src/app.ts`. If it equals `DASHBOARD_HOST`, the request goes to the Hono API at
`/api` and otherwise to the built dashboard SPA. Any other hostname is served as
an nsite.

`X-Forwarded-Host` takes precedence over `Host`, since a proxy that rewrites
`Host` would otherwise make every site unreachable. Ports and the trailing dot
of an FQDN are stripped and the name is lowercased (`requestHostname`,
`src/app.ts`). npanel cannot distinguish a proxy's `X-Forwarded-Host` from a
client's, so it must not be exposed directly.

Nothing is exempt from that rule, which is why certificate authorization is not
on this listener at all. Caddy's `on_demand_tls ask` is a plain request to the
address it proxies to, carrying no hostname npanel could route on: as a path
here it would have to answer on every hostname npanel serves, putting a
certificate-issuing decision on the public internet where a stranger could spend
a real host's hourly budget. So it gets a listener of its own on
`TLS_CHECK_PORT`, serving one path and nothing else (`src/tls-check.ts`). It
approves the hostnames npanel serves, plus the `www` names it only redirects —
a redirect is an HTTPS response like any other, and without a certificate the
browser never gets far enough to be told where to go. It
calls `Bun.serve` directly rather than going through Hono and the default export
in `src/server.ts`, which is what the runtime serves: one route needs no router,
and a second port has to be bound by hand.

A site only answers `GET` and `HEAD`; anything else is a `405`.

A host may also answer for its `www`. When nothing is configured for
`www.example.com`, npanel looks for `example.com`, and if that host has
`redirect_www` set — the default — the request gets a `301` to the same path
and query on the bare name. The redirect is offered only where the hostname is
a *root domain*, which is a question the Public Suffix List answers and label
counting does not: `example.co.uk` is one and `foo.github.io` is not. So
`src/hostname.ts` carries the list, via `tldts`, and the dashboard imports the
same module so it offers the switch exactly where the server will honour it.
Folding `www.docs.example.com` into `docs.example.com` would be a guess about
somebody else's zone, so npanel doesn't make it. A `www` host someone
configured deliberately always wins over the redirect.

The `301` is sent with an hour of freshness. Browsers cache a permanent
redirect with no freshness of its own until they restart, which would outlive
an admin turning the redirect back off; crawlers still take the `301` itself as
the permanent answer.

The dashboard is otherwise served straight off disk, with one exception:
`index.html` is read into memory and its `og:image` and `og:url` tags are
rewritten from site-relative paths to absolute URLs built from `DASHBOARD_HOST`
(`src/dashboard.ts`). A built static file cannot know its own origin, and
link-preview crawlers are inconsistent about resolving relative ones. The
result is cached until the file's mtime changes.

## The nsite model

A site is identified by a **pubkey**, a **kind**, and an **identifier**:

- kind `15128` — a root site. One per pubkey; the identifier is always `""`.
- kind `35128` — a named site, addressed by its `d` tag. A pubkey can publish
  many.

The manifest event lists the site's files as tags:

```json
["path", "/index.html", "<sha256>"]
["path", "/assets/app.js", "<sha256>"]
["server", "https://blossom.example.com"]
```

Each `sha256` is a [Blossom](https://github.com/hzrd149/blossom) blob, fetched
from `<server>/<sha256>`. Where those servers are comes from, in order: the
manifest's own `server` tags, the author's kind 10063 list, then the configured
fallbacks (`NostrService.blobSources`, `src/nostr.ts:141`).

`src/nsite-address.ts` accepts every form a person might paste — npub, nprofile,
naddr, an `a` tag, a NIP-5A label, a URL — and reduces it to
`{ pubkey, kind, identifier }`. The dashboard imports this same module so the
address preview in the UI can't drift from what the server will store.

## Sync

`SyncWorker` (`src/sync.ts`) is driven by three things:

1. **A live subscription** to the configured relays, filtered to the authors and
   kinds of every enabled host. The authors' own write relays (kind 10002) are
   merged in, since that's where they actually publish. New deploys usually
   land within seconds.
2. **A periodic resync** every `resync_interval` seconds, which re-fetches each
   site's newest manifest. This is the backstop for anything missed during relay
   downtime or a restart.
3. **Explicit triggers** — adding or retargeting a host, or the *Sync now*
   button, which calls `POST /api/hosts/:hostname/sync`.

A manifest for a site nobody has configured is ignored. One that is configured
is stored verbatim in `site_events`, its `path` tags are expanded into
`site_files`, and the version is queued for deployment.

Deploying means downloading every blob the version references, four at a time,
trying each candidate server in turn. Only when all of them are on disk does the
version become `ready`.

Downloads only go to public addresses, and there is no way to turn that off.
Download targets are not npanel's to choose — they come from the manifest's own
`server` tags, from the author's kind 10063 list, and from wherever those
servers redirect — so without the rule, publishing a manifest would be enough to
make npanel probe cloud metadata, a LAN admin interface, or a database's HTTP
port, with the resulting status codes landing in a deploy log the author can
read. Redirects are followed by hand, three at most, so each hop is resolved and
checked rather than only the first. The check resolves the name rather than
pinning the connection to the address it checked, so a determined DNS-rebinding
attack can still get through: don't run npanel somewhere a single GET request to
a neighbouring service would be a disaster.

That last part is the important invariant: **a host serves only `ready`
versions**. A deploy that is still downloading, or that failed outright, never
replaces a working site — the previous version keeps serving. A brand-new host
with no ready version returns `503` with a `Retry-After` instead of a broken
page.

A deploy that fails is tried again — `deploy_attempts` times, three by default,
waiting five seconds and then ten — before npanel leaves it to the resync timer.
The queue orders versions by when they may next be attempted, so a version
waiting out its backoff never delays one that is ready to go, and only an
otherwise empty queue actually sleeps. Retries are free of any special
bookkeeping because the blob store is content-addressed: whatever an attempt
managed to download is still there, so the next one asks only for what is
missing. The attempt count lives in memory and covers one run, so a resync or a
settings change starts a fresh one; that is what lets a site recover by itself
when the server holding its blobs comes back hours later.

The invariant survives shutdown, too. A deploy that stops with blobs it never
attempted is put back to `pending` rather than being marked ready or failed —
ready would serve a site with missing files and nothing retries a ready
version, and failed would be a lie about the site.

Every step also writes to `deploy_log`, which the dashboard shows per version:
which servers were tried, which blob failed and why, and the final verdict. A
failed deploy explains itself without anyone SSHing into the box.

## Storage

**SQLite** via Kysely, at `DATA_DIR/npanel.sqlite3`. Migrations run at startup.
The dialect in `src/db/sqlite.ts` is a thin adapter over whichever synchronous
binding the runtime has: `node:sqlite` if it's there — which now includes Bun
and Deno, not just Node — and `bun:sqlite` otherwise. Either way there's no
native module to compile. (`better-sqlite3` is deliberately avoided: it crashes Bun's
NAPI implementation.)

The tables:

| Table         | Holds                                                       |
| ------------- | ----------------------------------------------------------- |
| `users`       | Who may log in, and their role                              |
| `hosts`       | Hostname → `{pubkey, kind, identifier}`, pin, enabled, hidden, www, spa |
| `host_grants` | Which staff may manage which host                           |
| `site_events` | Every manifest seen, verbatim, with deploy status           |
| `site_files`  | Path → sha256, per version                                  |
| `blobs`       | Size and last-seen time per stored blob                     |
| `settings`    | Runtime configuration, JSON-encoded                         |
| `audit_log`   | Every change to a host, user, or setting                    |
| `deploy_log`  | Per-version deploy trace, capped at 500 entries             |

**Blobs** live on the filesystem at `DATA_DIR/blobs/<ab>/<cd>/<sha256>`, a
two-level fanout that keeps directories small. Downloads stream into
`blobs/tmp/` while being hashed and are renamed into place only if the hash
matches, so a truncated transfer or a lying server can't leave a bad blob
behind, and readers never see a partial file. Leftover temp files are cleared at
startup — they can only be the debris of a crash.

Because storage is content-addressed, a file shared between versions or between
sites is stored once, and rolling back to an old version usually downloads
nothing.

## Serving a site

`src/serve-site.ts` handles a request in a few steps: find the host, find its
active version, resolve the path, stream the blob.

Path resolution mirrors what static hosts do: exact match, then `index.html` for
extensionless paths, then the site's own `/404.html`. Resolution reads
`site_files`, never a manifest event — serving a file never parses Nostr.

That last step is where a single-page app parts ways with a static site, so it
is a per-host flag rather than a guess. By default a path the site has no file
for is a `404`, carrying `/404.html` if the site published one — the honest
answer, and the one crawlers, uptime checks and `curl -f` are entitled to. With
`spa` set, a *document* request (no extension, or `.html`) instead gets the
site's shell under a `200`: `/404.html` first, because NIP-5A names it as the
fallback path and an nsyte build publishes its shell there for exactly that
reason, then `/index.html`, for a build that expects the `try_files` every
other static host does. Requests for anything else keep their `404` in both
modes — a script or stylesheet answered with a page of HTML and a `200` doesn't
render the site, it turns a plain missing file into a parse error somewhere
else. The flag is off for every existing host and every new one: folding `www`
into a domain preserves what a response means and can default on, while turning
a `404` into a `200` does not.

Responses carry an `ETag` of the blob hash (so conditional requests are free and
exact), `Last-Modified` from the event's `created_at` — capped at when npanel
saw it, since an author can date a manifest however they please — and
`X-Npanel-Pubkey` / `X-Npanel-Event` headers identifying exactly what was
served. Range requests are supported, including suffix ranges.

Caching splits by what a file is. A site's URLs are stable across deploys while
the blobs behind them are not, so documents are sent `no-cache` and revalidate
against the `ETag` on every request; assets, which a revalidated document links
to by name, get an hour. Nothing is cached long enough to outlive a deploy.

Nothing is proxied at request time. Every byte comes from local disk, which is
why a served site survives its relays and Blossom servers going offline.

## Authentication

NIP-98: the client signs a kind 27235 event naming the method and URL, and sends
it base64-encoded in `Authorization: Nostr <event>`. npanel verifies the
signature, checks the event is less than 60 seconds old, that the method
matches, and — for requests with a body — that the `payload` tag matches the
body's sha256.

The signed `u` tag is compared against `DASHBOARD_HOST`, not against the URL
this process sees. Behind a proxy those differ: the client signs
`https://panel.example.com/api/hosts` while the process receives
`http://127.0.0.1:3000/api/hosts`. Comparing the raw request URL would reject
every legitimate request. Host and path must match exactly; scheme and port are
tolerated.

A valid signature proves identity, not authorization. npanel is not
permissionless, so a pubkey with no row in `users` gets `403`. Admins see and
change everything; staff see only the hosts they've been granted, and only
admins manage users, grants, settings, and the audit log.

What a host is *named* is a weaker secret than what it holds, so it is governed
separately. With `public_site_list` on, npanel will list its hostnames — to
anyone at `/api/sites`, and to staff alongside the hosts they manage — while
everything else about a host still needs a grant. The setting is off by default,
and a host marked `hidden` is left out of both regardless.

Two things happen before any of that. Request bodies are capped at 1 MB — the
`payload` check hashes the body, which means reading it before knowing who is
asking, and anyone can sign an event. And every dashboard and API response is
sent with `frame-ancestors 'none'`, since the SPA's own CSP is a `<meta>` tag
and those cannot express it.

## Bunkers

A bunker is an identity npanel can sign for over [NIP-46], and the point of it
is that npanel never holds the key.

Instead there are two hops. Upstream, npanel is a NIP-46 *client* of a signer
that really does hold the key — Amber on someone's phone, usually — and stores
only the client credential for that session. Downstream, npanel is a NIP-46
*signer* for connections that staff mint, and forwards what they ask to the
phone.

```
   client ──24133──► npanel bunker gateway ──24133──► Amber ──► signature
          per-connection key          upstream client key      the real key
```

Three things fall out of that shape, and they are the reasons for it:

- **No nsec is ever on the server.** What is stored is a credential to a
  session, which the phone can revoke at any time without anyone changing a key.
- **Re-pairing is invisible downstream.** Replacing the upstream credential
  re-points a bunker; every connection minted from it keeps its own keypair and
  never notices.
- **Revoking one client is a row delete.** Each connection answers on a keypair
  of its own rather than on the bunker's, so removing one breaks nothing else,
  and two clients can't recognise each other by the pubkey they talk to.

Credentials are encrypted at rest with [`SECRET_KEY`](configuration.md#secret_key),
using NIP-44's payload format as a sealed box. Each bunker records which key
sealed it, so starting with the wrong one is reported as such instead of
surfacing later as a decryption failure. With no key set, the whole section
answers `503` and says why — there is no degraded mode that writes credentials
somewhere less safe.

### Why a worker thread

Serving nsites is what npanel is for; bunkers are what it also does. The gateway
therefore runs in its own thread (`src/bunker/worker.ts`) with its own SQLite
connection — WAL and a busy timeout make that safe — and its own relay pool.

Nothing it does can reach the request path. A crash takes down the thread and
not the process: the host handle rejects whatever was in flight, logs
`bunker_worker_exited`, and respawns with a capped backoff. A hang costs a
bunker route its response and nothing else. A runtime that cannot spawn it at
all leaves every site still being served.

The split also bounds relay sockets, which is the other reason. The gateway's
pool is separate from the sync worker's, and it is deliberately frugal with it:
subscriptions are keyed by relay rather than by connection, so every connection
on a relay shares one filter and one socket, and `bunker_relays` caps how many
relays there are to hold open in the first place.

### Permissions

Access to a bunker follows the same rule as a host: admins see all of them,
staff see the ones they've been granted, and whoever creates one is granted it.

What a *connection* may do is a static allow-list, in NIP-46's own
`method[:kind]` spec format, fixed when the connection is minted and editable
afterwards by anyone with access to the bunker. There is no approval prompt,
because there is nobody at it — a request outside the grant is refused
immediately and logged, rather than left hanging until the client times out.
`connect`, `ping`, `get_public_key`, `switch_relays` and `logout` need no grant;
nothing else is implicit.

Upstream, npanel asks the phone for everything. Narrowing the request there
would only mean a grant made in the dashboard failing later on a device nobody
is watching — the enforcement point is here, per connection, on purpose. Which
means the signer should be told to trust npanel standing: with a prompt-per-
request policy, every downstream signature waits on a tap and most will time
out.

[NIP-46]: https://github.com/nostr-protocol/nips/blob/master/46.md

## Live settings

`SettingsService` caches settings in memory and emits a change event on write.
Subscribers reconfigure in place: the relay pool drops connections to relays
that are gone and clears its list caches, the subscription restarts, the resync
timer re-arms, and adding Blossom fallbacks or raising the size limit
immediately retries every version that isn't ready — because that's often
exactly what a failed deploy was waiting for.

Nothing in the settings UI requires a restart, which is the point: the
environment variables are the only things that do.

The bunker worker is the one subscriber that isn't in this process. It holds its
own cache, read from the same table through its own connection, so a write here
is pushed across as a `reload` rather than heard as an event.
