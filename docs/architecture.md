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

Only `Host` is read. Ports and the trailing dot of an FQDN are stripped and the
name is lowercased (`requestHostname`, `src/hostname.ts`, shared with the Tor
listener so the two cannot drift). `X-Forwarded-Host` is deliberately ignored:
npanel cannot tell a proxy's copy of that header from a client's, and a proxy
that doesn't set one — nginx doesn't — would pass a visitor's straight through,
letting a request for any site ask for the dashboard instead. A proxy that
rewrites `Host` has to be configured not to; see
[deployment](deployment.md#other-proxies).

The scheme is a different matter, and comes from `X-Forwarded-Proto`
(`requestScheme`, `src/app.ts`), defaulting to `https`. npanel terminates no
TLS, so the local request always says `http` and redirects built from it would
downgrade; believing the proxy here costs at worst a redirect to the scheme the
visitor was already using.

Both are settled in that one middleware and handed to the site server as a
`RequestOrigin` — nothing downstream reads a header to work out where a request
came from, which is what lets the same site-serving code be reached from a
listener with entirely different rules about what may be trusted.

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
| `hosts`       | Hostname → `{pubkey, kind, identifier}`, pin, enabled, hidden, www, spa, csp |
| `host_grants` | Which staff may manage which host                           |
| `site_events` | Every manifest seen, verbatim, with deploy status           |
| `site_files`  | Path → sha256, per version                                  |
| `blobs`       | Size and last-seen time per stored blob                     |
| `settings`    | Runtime configuration, JSON-encoded                         |
| `audit_log`   | Every change to a host, user, or setting                    |
| `deploy_log`  | Per-version deploy trace, capped at 500 entries             |
| `onion_services` | A host's Tor address and the key it is named by          |

**Blobs** live on the filesystem at `DATA_DIR/blobs/<ab>/<cd>/<sha256>`, a
two-level fanout that keeps directories small. Downloads stream into
`blobs/tmp/` while being hashed and are renamed into place only if the hash
matches, so a truncated transfer or a lying server can't leave a bad blob
behind, and readers never see a partial file. Leftover temp files are cleared at
startup — they can only be the debris of a crash.

Because storage is content-addressed, a file shared between versions or between
sites is stored once, and rolling back to an old version usually downloads
nothing.

**Compressed copies** sit beside their blob as `<sha256>.br` and `<sha256>.gz`,
written during the deploy that downloads it. Compressing once at deploy time
rather than on every request is the whole point: a gateway serves the same
bytes to everyone, so paying for brotli's slowest setting once buys a smaller
transfer for every visitor afterwards, including the ones the proxy in front
of npanel would never have reached.

Whether a blob is worth compressing is decided from the paths it is served
under, because the store is keyed by hash and so can't tell markup from a JPEG
— a blob is compressed if any path referencing it has a content type that is
text underneath. A copy is kept only if it beats the blob by a real margin, and
blobs under half a kilobyte are skipped: they arrive in one packet either way.

A sidecar is an attribute of its blob, not a blob of its own. Nothing
references one by name, `BlobStore.list()` doesn't yield them — the name test
that keeps stray files out of pruning excludes them, since they aren't 64 hex
characters — and deleting a blob deletes them with it. Pruning therefore goes
on working entirely in terms of hashes.

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
saw it, since an author can date a manifest however they please — and an
`X-Nsite-Addr` header naming the nsite the hostname resolved to. Range requests
are supported, including suffix ranges.

Which of a blob's stored copies gets sent is settled before any of that. The
client's `Accept-Encoding` is honored as the ranking it is — zero means refused
— but ties are broken by size rather than by a preference of npanel's own, and
ties are the case that happens: browsers offer their encodings at equal
quality, so a client is sent the smallest copy it can read. Tor Browser
reaching an onion offers only `gzip`, because Firefox withholds `br` from plain
HTTP, and gets the gzip copy. A client that accepts nothing stored gets the
blob; there is no 406, since for a static file a slower answer beats no answer.

Everything about the response then describes the copy that won, not the blob:
its length, its `ETag`, and the bytes a `Range` counts from. The `ETag` has to
be per encoding — one tag across all three would let a shared cache holding the
brotli copy hand it to the next client whose `If-None-Match` matched, however
little brotli that client speaks — and `Vary: Accept-Encoding` goes on every
site response, including ones that negotiated nothing, because a cache has to
be told before it stores the first response rather than after.

This is why the sample `Caddyfile` has no `encode` on the site block. There is
nothing left for the proxy to compress, and a proxy could only ever help the
visitors who arrive through it — not the ones on a site's onion address.

Sites are also served under a `Content-Security-Policy`. Which one is the site's
to say: if its `/index.html` declares a policy in a `<meta http-equiv>` tag,
that is what npanel sends, on every response for the host. A site that declares
nothing gets the gateway's, which is the same policy mkstack puts in its
`index.html` — a static site is exactly the thing with no business running an
inline script, and `script-src 'self'` is what keeps a compromised deploy or a
careless `innerHTML` from being an XSS on an origin your visitors, and their
Nostr keys, trust.

Either way it goes out as a header rather than being left to the meta tag,
because a meta tag is only read once the parser reaches it and doesn't apply to
responses that aren't HTML. A declared policy replaces the gateway's rather than
joining it, which means a site can widen its own — that is the author's call
about their own hostname, and it reaches no other host. The policy is read from
`/index.html` on the first request for a version and cached against its
`event_id`, which is safe because a version's file list is written once and
never edited.

The `csp` flag, which defaults on, turns the header off entirely for one host —
for a site that needs something neither policy allows. It is per host and there
is no instance-wide equivalent, on purpose: a global off switch makes disarming
every site at once easier than fixing the one site that needed it.

Caching splits by what a file is. A site's URLs are stable across deploys while
the blobs behind them are not, so documents are sent `no-cache` and revalidate
against the `ETag` on every request; assets, which a revalidated document links
to by name, get an hour. Nothing is cached long enough to outlive a deploy.

Nothing is proxied at request time. Every byte comes from local disk, which is
why a served site survives its relays and Blossom servers going offline.

## Tor

With `tor_enabled` on, every enabled site is also published as a v3 onion
service. Staff do nothing: adding a site mints an address for it, removing one
takes its address away with it.

npanel drives a Tor daemon over its control port rather than writing `torrc`.
`ADD_ONION` registers a service immediately, with no restart and no
configuration file shared between two processes, and it hands back the key Tor
generated. **npanel keeps the keys and Tor keeps nothing**: services are
registered at every connect and are deliberately not created with
`Flags=Detach`, so they disappear with the control connection. That is the
right way round — the key is what an address is made of, so storing it is what
makes the address survive a restart, while an onion that outlived npanel would
answer at an address whose server is gone.

Sites are *reconciled*, not tracked. Connecting, adding or removing a host, and
changing a setting all run the same routine: ask the database which hosts are
enabled, and make Tor match. Nothing is remembered between two runs, so a
reconcile interrupted halfway is repaired by the next one instead of leaving a
site holding an address nobody serves. An unreachable Tor is retried with
backoff, and everything is republished from stored keys when it comes back.

**Onion traffic gets its own listener, and this is not optional.** Tor forwards
a visitor's bytes unaltered, so every header in an onion request is the
visitor's to choose — including `Host`, which on the public listener is enough
to ask for the dashboard. This listener serves nothing but `.onion` names, so
the dashboard and the JSON API cannot be reached through an onion however a
request dresses itself up. Same reasoning that put certificate authorization on
its own port.

It also states its `RequestOrigin` outright — `scheme: "http"`, because there
is no proxy and no TLS anywhere on this path — rather than reading
`X-Forwarded-Proto`, which here would be a header the visitor wrote. Tor
authenticates and encrypts the connection itself, which is what the scheme
would otherwise be claiming.

That listener binds loopback on a port the kernel picks. npanel is the one
issuing `ADD_ONION`, so it tells Tor which port to forward to; nobody else ever
needs the number, and there is nothing to configure or collide with.

One consequence elsewhere: `.onion` is in the Public Suffix List, so an onion
address would otherwise read as a root domain and be offered a `www` redirect
into nothing — `isRootDomain` excludes them.

Clearnet HTML responses carry `Onion-Location`, which is how Tor Browser offers
the address to a visitor without anyone having to hand it out. It is sent only
over HTTPS, because on a plain-HTTP hop anything in the path could rewrite it
and point the visitor at an onion of its own choosing.

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

## Live settings

`SettingsService` caches settings in memory and emits a change event on write.
Subscribers reconfigure in place: the relay pool drops connections to relays
that are gone and clears its list caches, the subscription restarts, the resync
timer re-arms, and adding Blossom fallbacks or raising the size limit
immediately retries every version that isn't ready — because that's often
exactly what a failed deploy was waiting for.

Nothing in the settings UI requires a restart, which is the point: the six
environment variables are the only things that do.
