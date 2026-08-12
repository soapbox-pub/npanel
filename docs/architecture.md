# Architecture

npanel is one process that does three things: it serves nsites over HTTP, it
serves a dashboard for configuring which nsites those are, and it runs a
background worker that keeps local storage in sync with Nostr.

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
                                  │                            │
                    relays (manifests) ── Blossom (blobs) ─────┘
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

`/api/tls-check` is the one exception to the hostname rule. Caddy asks it with
a plain request to the address it proxies to — `Host: 127.0.0.1:3000` — so
routing it by hostname would serve it as an unconfigured nsite and deny every
certificate.

A site only answers `GET` and `HEAD`; anything else is a `405`.

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
   button, which calls `POST /api/hosts/:id/sync`.

A manifest for a site nobody has configured is ignored. One that is configured
is stored verbatim in `site_events`, its `path` tags are expanded into
`site_files`, and the version is queued for deployment.

Deploying means downloading every blob the version references, four at a time,
trying each candidate server in turn. Only when all of them are on disk does the
version become `ready`.

Downloads only go to public addresses. A manifest names its own servers, so
without that rule publishing one would be enough to make npanel probe whatever
is listening on its own network; `allow_private_blob_servers` turns the check
off for people whose Blossom server is on the LAN. Redirects are followed by
hand, three at most, so each hop is checked rather than only the first.

That last part is the important invariant: **a host serves only `ready`
versions**. A deploy that is still downloading, or that failed outright, never
replaces a working site — the previous version keeps serving. A brand-new host
with no ready version returns `503` with a `Retry-After` instead of a broken
page.

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
| `hosts`       | Hostname → `{pubkey, kind, identifier}`, pin, enabled flag   |
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
extensionless paths, then the site's own `/404.html` (served with a real `404`
status). Resolution reads `site_files`, never a manifest event — serving a file
never parses Nostr.

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

Two things happen before any of that. Request bodies are capped at 1 MB — the
`payload` check hashes the body, which means reading it before knowing who is
asking, and anyone can sign an event. And every dashboard and API response is
sent with `frame-ancestors 'none'`, since the SPA's own CSP is a `<meta>` tag
and those cannot express it.

## Live settings

`SettingsService` caches settings in memory and emits a change event on write.
Subscribers reconfigure in place: the relay pool drops connections to relays
that are gone and clears its list caches, the subscription restarts, the resync
timer re-arms, and adding Blossom fallbacks or raising the size limit
immediately retries every version that isn't ready — because that's often
exactly what a failed deploy was waiting for.

Nothing in the settings UI requires a restart, which is the point: the five
environment variables are the only things that do.
