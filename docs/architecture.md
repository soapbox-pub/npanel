# Architecture

npanel is one process that does three things: it serves nsites over HTTP, it
serves a dashboard for configuring which nsites those are, and, on threads of
its own, it keeps local storage in sync with Nostr.

```
        DNS ──► reverse proxy (TLS) ──► npanel :3000
                 or, with HTTPS_PORT ──► npanel :443, its own TLS
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

   /_npanel/api/event on any site ──► analytics thread ──► analytics.sqlite3
   a crawler on a page ──► previews thread ──► the site's script ──► relays
   a site's onion address ──► Tor, built in, on its own thread ──► the same site
   with HTTPS_PORT: SQLite ◄── ACME, on its own thread ◄──► the CA, via :80 and :443
```

## Request routing

Everything hangs off the `Host` header, checked in one middleware in
`src/app.rs`. If it equals `DASHBOARD_HOST`, the request goes to the axum API at
`/api` and otherwise to the built dashboard SPA. Any other hostname is served as
an nsite.

Only `Host` is read. Ports and the trailing dot of an FQDN are stripped and the
name is lowercased (`request_hostname`, `src/hostname.rs`, shared with onion
connections so the two cannot drift). `X-Forwarded-Host` is deliberately ignored:
npanel cannot tell a proxy's copy of that header from a client's, and a proxy
that doesn't set one — nginx doesn't — would pass a visitor's straight through,
letting a request for any site ask for the dashboard instead. A proxy that
rewrites `Host` has to be configured not to; see
[deployment](deployment.md#other-proxies).

The scheme is a different matter. Behind a proxy it comes from
`X-Forwarded-Proto` (`request_scheme`, `src/app.rs`), defaulting to `https`:
the local request always says `http`, and redirects built from it would
downgrade, so believing the proxy here costs at worst a redirect to the scheme
the visitor was already using. Without a proxy — see
[TLS without a proxy](#tls-without-a-proxy) — the listener that terminated TLS
says `https` itself, and no forwarded header is read at all.

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
`TLS_CHECK_PORT`, serving one path and nothing else (`src/tls_check.rs`). It
approves the hostnames npanel serves, plus the `www` names it only redirects —
a redirect is an HTTPS response like any other, and without a certificate the
browser never gets far enough to be told where to go. It is a bare handler
rather than a router, since one route needs none.

A site only answers `GET` and `HEAD`; anything else is a `405`.

A host may also answer for its `www`. When nothing is configured for
`www.example.com`, npanel looks for `example.com`, and if that host has
`redirect_www` set — the default — the request gets a `301` to the same path
and query on the bare name. The redirect is offered only where the hostname is
a *root domain*, which is a question the Public Suffix List answers and label
counting does not: `example.co.uk` is one and `foo.github.io` is not. So
`src/psl.rs` carries the list's ICANN section, and the dashboard's twin of
`src/hostname.rs` applies the same list (via `tldts`) so it offers the switch
exactly where the server will honour it.
Folding `www.docs.example.com` into `docs.example.com` would be a guess about
somebody else's zone, so npanel doesn't make it. A `www` host someone
configured deliberately always wins over the redirect.

The `301` is sent with an hour of freshness. Browsers cache a permanent
redirect with no freshness of its own until they restart, which would outlive
an admin turning the redirect back off; crawlers still take the `301` itself as
the permanent answer.

The dashboard is otherwise served as built, with one exception: `index.html`
has its `og:image` and `og:url` tags rewritten from site-relative paths to
absolute URLs built from `DASHBOARD_HOST` (`src/dashboard.rs`). A built static
file cannot know its own origin, and link-preview crawlers are inconsistent
about resolving relative ones.

Where the files come from depends on the build. A release binary is built with
the `embedded-dashboard` feature, and `build.rs` compiles every file of
`dashboard/dist` into it, so a server and its dashboard can't come from
different versions. Without the feature they are read off disk, from
`dashboard/dist` under the working directory, beside the binary, or in the
source tree, and the rewritten `index.html` is cached until its mtime changes,
which is what lets a developer rebuild the dashboard under a running server.

Its text files are served compressed, negotiated the way a site's are below,
with the same brotli and gzip settings and per-encoding `ETag`s. The copies are
made in memory rather than at build time: `dist` is also published as an
nsite, which shouldn't carry them, and `index.html` is only final once it has
been rewritten. Brotli at its slowest takes seconds over the larger chunks, so
npanel compresses every file on the blocking pool when it starts, and a file
asked for before its copies are ready goes out as it is, in the meantime. A
file that changes on disk is compressed again the first time it is asked for.

The same build is also published as an nsite, where nothing serves the API on
its origin. It finds its server at startup (`dashboard/src/lib/server.ts`): if
`/api/instance` on its own origin answers as npanel, that is the server;
otherwise it asks the reader for one, and remembers it. Every API response
allows any origin, and NIP-98 binds a request to the API's URL rather than the
page's, so a dashboard on another origin needs nothing else from the server.

## TLS without a proxy

With `HTTPS_PORT` set npanel terminates TLS itself, and gets every certificate
it serves from an ACME certificate authority (`src/tls/`). It is optional: the
proxy path above is unchanged, and nothing about serving a site differs between
the two beyond where the bytes are encrypted.

**Which names get one is the same rule either way.** `src/tls/names.rs` answers
it name by name for Caddy's `/tls-check` and all at once for npanel's own
client: the dashboard hostname, every enabled host, and the `www` of each root
domain that redirects it. Two answers to one question can't drift if one
function gives both.

**Certificates are reconciled**, like builds and onions. Startup, a host being
added, renamed, enabled or disabled, a setting changing, and a timer all run
the same routine on the TLS runtime's thread: read the names, read what is
stored, serve exactly the stored certificates that are wanted and unexpired,
then order any name that is due — one without a certificate, or one with a
third of its lifetime left — one at a time. Nothing is remembered between two
runs, so the runtime restarts as cheaply as the others.

A name that fails waits before its next attempt — two minutes, doubling to an
hour — and the backoff is stored with it, so a restart doesn't start it over.
That pacing is what keeps a name whose DNS points elsewhere within Let's
Encrypt's five failed validations an hour. Each attempt validates by one
challenge, alternating between TLS-ALPN-01 and HTTP-01, rather than trying both
and spending two. A renewal that fails leaves the old certificate serving until
it expires. The two hourly limits that meter `/tls-check` meter orders here,
and the dashboard host is exempt from them in both.

**npanel keeps the keys.** Certificates, their private keys and the ACME
account are rows in the database, kept per CA directory, as onion keys are. A
database backup is a complete one, and a restart serves what is stored from
the first connection, loaded before the listener opens. Switching to the
staging CA and back finds production's certificates where they were.

**Handshakes never wait.** The listener's certificate resolver reads an
`ArcSwap` map by SNI and nothing else; every change to it is made on the TLS
runtime and swapped in whole. A name with no certificate — not yet, or not
ever — gets no handshake at all rather than some other name's certificate. The
listener offers `h2` and `http/1.1`, and a CA validating TLS-ALPN-01 offers
only `acme-tls/1`, which is answered with that name's challenge certificate
and closed once the handshake is done.

**Plain HTTP serves nothing.** `PORT` answers HTTP-01 challenges and sends
every name npanel would serve to the same path over HTTPS with a `308`, kept
for an hour like the `www` redirect; any other name is a `404`, so the
redirect can't be pointed elsewhere.

**No proxy means no forwarded headers.** A request off the TLS listener is
handed to the app as `Via::Tls`, which fixes its scheme at `https` and its
visitor at the address that connected (`Peer::Visitor`). `X-Forwarded-For`
is the visitor's own to write there, so analytics no longer read it — as over
Tor, where there is no address at all (`Peer::Unknown`).

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
fallbacks (`blob_sources`, `src/nostr/service.rs`).

`src/nsite_address.rs` accepts every form a person might paste — npub, nprofile,
naddr, an `a` tag, a NIP-5A label, a URL — and reduces it to
`{ pubkey, kind, identifier }`. The dashboard has a TypeScript twin of it
(`dashboard/src/shared/nsite-address.ts`) so the address preview in the UI
matches what the server will store; the two are changed together.

## Sync

`SyncWorker` (`src/sync.rs`) is driven by three things:

1. **A live subscription** to the configured relays, filtered to the authors and
   kinds of every enabled host. The authors' own write relays (kind 10002) are
   merged in, since that's where they actually publish. New deploys usually
   land within seconds.
2. **A periodic resync** every `resync_interval` seconds, which re-fetches each
   site's newest manifest. This is the backstop for anything missed during relay
   downtime or a restart.
3. **Explicit triggers** — adding or retargeting a host, or a site's *Check for
   a new version* button, which calls `POST /api/hosts/:hostname/sync`.

A manifest for a site nobody has configured is ignored. One that is configured
is stored verbatim in `site_events`, its `path` tags are expanded into
`site_files`, and the version is queued for deployment.

Deploying means downloading every blob the version references, four at a time,
trying each candidate server in turn. Only when all of them are on disk does the
version become `ready`.

What gets downloaded is the author's choice, not staff's, so it is bounded.
Only a site's newest version is deployed — one that a newer manifest overtook
while it waited fails as superseded — and a version over `max_site_files` or
`max_site_size` fails without being retried. An author publishing manifests as
fast as they can is downloaded one bounded version at a time, not one version
per manifest. Superseded versions hold their blobs until the daily prune
removes them (see [pruning](#pruning)).

Downloads only go to public addresses, and there is no way to turn that off.
Download targets are not npanel's to choose — they come from the manifest's own
`server` tags, from the author's kind 10063 list, and from wherever those
servers redirect — so without the rule, publishing a manifest would be enough to
make npanel probe cloud metadata, a LAN admin interface, or a database's HTTP
port, with the resulting status codes landing in a deploy log the author can
read. Redirects are followed by hand, three at most, so each hop is resolved and
checked rather than only the first. The check is the resolver the HTTP client
connects through (`PublicResolver`, `src/net.rs`), so the address checked is
the address dialed and a DNS-rebinding name gets no second answer.

Relays the author lists in their kind 10002 are held to the same rule, since
they are just as much the author's to choose and each one is a WebSocket the
deploy runtime opens. Only `wss://` URLs on public addresses are used, and only
the first five (`vet_relay_hints`, `src/nostr/service.rs`). As with downloads,
the relay connection is checked where it is made: the pool resolves the name
itself and dials the addresses it checked (`src/nostr/pool.rs`), so a
rebinding name gets no second answer.
The relays an admin configures
are not checked against the address rule — pointing npanel at a relay on the
LAN is theirs to decide — but they too must be `wss://`.

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

## Builds

A ready version is not yet something a host serves. What a host serves is its
**build**: the finished bundle for that host, made once and then frozen
(`HostBuilder`, `src/builds.rs`). A build is a path map — the host's version
chain flattened into one, plus every file npanel generates or rewrites for that
host: a sitemap, a `robots.txt` with a line appended, the `window.ENV` script
for a host with variables, the tracker script with analytics on, and every page
rewritten to load them — and everything in it is an
ordinary stored blob with its compressed copies. So every body a site sends is a slice of a kept memory
mapping of a stored file, including the ones npanel wrote, and all the work of making them happens once, when the host
is built, rather than on a request. Only headers are decided per request.

Every script npanel adds to a page — `window.ENV` and the tracker — is loaded
at a URL versioned by its contents, `/_npanel/script.js?v=<hash>`. The path
answers whatever the query says, and the tracker is cached for an hour like any
other asset; the version is what lets a change reach visitors at once, since
the pages naming it revalidate on every request and each change names a URL no
cache has seen. An `ETag` alone would not do it: nothing asks for the script
again until its hour is up. The env script is cached differently, for reasons
of its own — see [environment variables](#environment-variables).

The invariant goes up a level with it: **a host serves only its newest ready
build**. A version that finishes deploying goes live on a host once that
host's build of it is ready, and a build that fails leaves the previous one
serving and says why in the version's deploy log. A host that has never been
built answers 503, exactly as one whose site has never deployed.

Builds are keyed by host rather than by version, because the output depends on
the host: its hostname is named in the sitemap, and its settings decide what is
generated at all. An onion gets a build of its own for the same reason — a
sitemap fetched over Tor lists onion URLs. What the two share is stored once,
being content-addressed; only the rows are doubled.

Hosts are reconciled, not tracked, the way Tor's onions are. A deploy
finishing, a host being edited, an onion being minted, and startup all ask the
same question: what should each host's build be made of? The answer — the
builder's version, the origin, the version chain, and the flags that shape the
output — is hashed into a fingerprint, and a host is rebuilt only where that
differs from the build it is serving. Flipping a switch that only changes
headers (`csp`, `spa`) therefore rebuilds nothing, and a new npanel whose
builds would come out differently bumps `BUILDER_VERSION` and rebuilds
everything at startup. A rename keeps the old build serving until the new one
names the new hostname.

A build carries every file row it serves and references its versions by id
only, so pruning the versions it was made from cannot take a working site down.
Each host keeps its current build and the one before it — a request that
looked up the old one a moment before the switch may still be resolving a path
in it — and older ones are deleted as the new one goes live.

## The deploy runtime

Sync and builds run on a Tokio runtime of their own, on threads of their own
(`src/deploy.rs`), starting from the relay pool. Everything expensive about
deploying a site happens there: checking the signature of every event a relay
sends, downloading and hashing every blob, compressing at brotli's slowest
setting, rewriting pages and generating files for a build, and the database
writes that record each step. None of it is on the threads that serve sites,
which go on answering requests for the whole of a large deploy.

The two runtimes share the settings (a change is broadcast to both) and the
serving cache (the deploy runtime clears it when a build goes live), and write
through the same pool of database connections. So very little has to cross the
channel between them: the API says when a host was edited or someone pressed
*Check for a new version* (`DeployService`).

None of what the API says has to arrive. Sync and builds are reconciled rather
than tracked, so a runtime starting up does everything a missed message would
have asked for: it resyncs every site and rebuilds every host whose build is out
of date. That is what makes it cheap to restart. A panic that escapes it ends
it and it is started again with backoff, and whatever is sent while it starts
waits in the channel until it is ready. A panic inside one deploy or one build
is caught where it happens and fails that version or that host, rather than
the queue behind it.

The price is a second SQLite writer. WAL keeps reads from ever waiting on a
write, but two writes still take turns, and every query runs on Tokio's
blocking pool so a wait never holds up a runtime thread. Serving never writes,
the API writes only when staff change something, and the deploy runtime's
transactions are short — the longest inserts one build's file rows — so the
worst it costs is a dashboard save waiting a moment, never a visitor.

## Storage

**SQLite** via `rusqlite`, with SQLite compiled in, at
`DATA_DIR/npanel.sqlite3`. Migrations run at startup
(`src/db/migrations.rs`). Analytics live in a database of their own
beside it — see [analytics](#analytics). Connections are pooled
(`src/db/mod.rs`); each one maps the database into memory and keeps its
prepared statements. The pool is bounded at sixteen, and a query arriving
when all are in use waits for one rather than opening another — opening a
connection costs far more than any query npanel makes, and a burst of them,
like every hostname missing the serving cache the moment a build goes live,
would otherwise open a connection per query. Async callers wait their turn
before reaching the blocking pool, so a burst queues as futures rather than
as parked threads.

The tables:

| Table         | Holds                                                       |
| ------------- | ----------------------------------------------------------- |
| `users`       | Who may log in, and their role                              |
| `hosts`       | Hostname → `{pubkey, kind, identifier}`, pin, enabled, hidden, www, spa, csp, seo, merge |
| `host_grants` | Which staff may manage which host                           |
| `site_events` | Every manifest seen, verbatim, with deploy status           |
| `site_files`  | Path → sha256, per version                                  |
| `host_builds` | What each host serves: a frozen bundle, per host and name kind |
| `host_build_files` | Path → sha256, per build, and where each file came from |
| `blobs`       | Size, sidecar sizes, and last-seen time per stored blob     |
| `settings`    | Runtime configuration, JSON-encoded                         |
| `audit_log`   | Every change to a host, user, or setting                    |
| `deploy_log`  | Per-version deploy trace, capped at 500 entries             |
| `onion_services` | A host's Tor address and the key it is named by          |
| `nip05_names` | The NIP-05 names a host answers for                        |
| `host_env`    | The variables a host's pages read as `window.ENV`           |

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

Once a deploy has settled which copies a blob has, their sizes are recorded on
its `blobs` row — including the settled absence of one, which is `-1` and not
`NULL`, because "no copy is kept" and "nobody has looked yet" must not read the
same. The sizes then ride along on the `site_files` lookup a request already
makes, so serving answers `Accept-Encoding` without touching the disk; a `NULL`
row falls back to statting the sidecars until the next deploy that references
the blob — or one run of `npanel compress-blobs` — fills it in.

### Pruning

Nothing would otherwise ever be deleted: every version of every site stays,
with its files. So once a day, ten minutes after starting, and whenever
`keep_versions` changes, the deploy runtime prunes (`src/prune.rs`). The
`prune-blobs` command runs the same code on demand.

It forgets versions first, in one transaction: every version of a site no host
points at, and each site's ready versions past the newest `keep_versions`. Only
ready versions count, because they are what a site can roll back to. One that
never got there and is older than the newest ready version can never be
deployed — only a site's newest is — so it goes too. Anything newer may still
be deploying and is left alone, as are the version a host serves, its pin, and
every version a build names.

A merged build holds the files of every version in its chain, so forgetting
versions frees nothing on that host until it is rebuilt with the shorter
chain. Every prune therefore asks the builder to reconcile every merged host,
which rebuilds only those whose chain actually changed — including after a
`prune-blobs` run, which can't reach a running server's builder.

Then it deletes every blob nothing references. Listing the store is the slow
part and holds no connection; the deleting happens five hundred blobs to a
write transaction, and each blob is asked again, inside it, whether any
version or build references it or it was recorded in the last hour. The file
is gone before the transaction commits. That is what makes pruning safe beside
a deploy that comes to need a blob pruning has picked: a manifest naming it
either committed first, and is seen, or waits on the transaction, and its
deploy finds the blob missing and downloads it again. A version is never
marked ready on a blob pruning has just deleted. The hour is for a build,
which stores the files it generates before it writes the rows naming them.

## Serving a site

`src/serve.rs` handles a request in a few steps: find the host, find its
build, resolve the path, send the blob.

Path resolution mirrors what static hosts do: exact match, then `index.html` for
extensionless paths, then the site's own `/404.html`. Resolution reads
`host_build_files`, never a manifest event — serving a file never parses Nostr.

The answer is then held in memory, keyed by build and path, which is what
takes the last database query off a warm request. Nothing invalidates it and
nothing has to: a build's files are written once and never edited, and a
build id is never reused, so a rebuild asks a different question rather than
changing the answer to this one. The blob sizes that ride along are the
exception, since `compress-blobs` can record them long after a deploy, so a
row that hasn't got them yet is left uncached rather than pinned to the `stat`
it falls back to.

That last step is where a single-page app parts ways with a static site, so it
is a per-host flag rather than a guess. By default a path the site has no file
for is a `404`, carrying `/404.html` if the site published one — the honest
answer, and the one crawlers, uptime checks and `curl -f` are entitled to. With
`spa` set, a *document* request (no extension, or `.html`) instead gets the
site's shell under a `200`: `/404.html` first, because NIP-5A names it as the
fallback path and an nsyte build publishes its shell there for exactly that
reason, then `/index.html`, for a build that expects the `try_files` every
other static host does. A path whose last segment has an `@` in it —
`/@bob@example.com`, the way Nostr and fediverse clients route a NIP-05 name or
a handle — is a document too: its dot is a domain's, not an extension. Requests for anything else keep their `404` in both
modes — a script or stylesheet answered with a page of HTML and a `200` doesn't
render the site, it turns a plain missing file into a parse error somewhere
else. `/.well-known/` skips every fallback, in both modes: what asks there is
software probing for a capability — Universal Links, WebFinger, Matrix — and
any page at all, let alone one under a `200`, tells it the site has one. Those
paths also get their content types by name (`src/mime.rs`), since the
registered ones mostly have no extension. The flag is off for every existing host and every new one: folding `www`
into a domain preserves what a response means and can default on, while turning
a `404` into a `200` does not.

The `merge_deployments` flag, also off by default, changes what a build is
made of rather than what resolution does. A merged host's build flattens a
*chain* of versions — the one it serves, and every earlier ready version of the
same site underneath it, newest first — taking each path from the newest
version that has it (`version_chain`, `src/sites.rs`). The served version
therefore wins everywhere it published a file, and a path it dropped goes on
being answered by the deploy that last published it. Since resolution sees the
flattened map, an exact match in a superseded version still beats an
`index.html` in the served one, exactly as it would within a single version.

This is for builds that load modules by name at runtime. A Vite site names its
chunks by content hash, and a page a visitor already has open goes on asking
for the names of the deploy it was loaded from — which are the files the next
deploy removes. Without merging, a deploy mid-session breaks every open page.

Nothing newer than the served version is in the chain, so a pinned host merges
up to its pin rather than past it, and only `ready` versions are in it: an
unfinished deploy's blobs may not be on disk, and merging must not undo the
invariant that serving never reaches one. The chain is capped, because a build
writes a row for every path anywhere in it. Pruning is what bounds it in
practice: the daily prune forgets old versions first, so the next build
reaches exactly as far back as the versions still in the database — and it
deletes only blobs no remaining version or build references, so a build can
never resolve a path to a blob pruning took away.

What merging deliberately doesn't touch is the generated `sitemap.xml`, which
lists the served version's own pages. Keeping a withdrawn URL *answering*, for
the pages and caches still asking for it, is not a reason to invite a crawler
to index a page the site's latest deploy deleted.

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
the build's `/index.html` on the first request for it and cached against the
build, which is safe because a build is written once and never edited — and
reading the build rather than the manifest means a document npanel rewrote and
the header it goes out under can't disagree.

The `csp` flag, which defaults on, turns the header off entirely for one host —
for a site that needs something neither policy allows. It is per host and there
is no instance-wide equivalent, on purpose: a global off switch makes disarming
every site at once easier than fixing the one site that needed it.

Sites also get their standing with search engines looked after, from data
npanel already holds, and all of it made when the host is built. A site that
published no `/sitemap.xml` gets one generated from the version's file list —
its HTML paths in canonical form, dated by the version — and a site without
`/robots.txt` gets one that points at the sitemap, which is how
crawlers find it without anyone registering anything. The rule is the NIP-05
one in reverse: only where the site published no file at that exact path, and a
site's own `/404.html` standing in for the missing path doesn't count as
publishing one.

`robots.txt` is then the one exception to that rule, because standing in for a
missing file turned out to cover almost nobody: nearly every nsite ships the
boilerplate `User-agent: * / Allow: /`, which names no sitemap and blocks the
generated one from ever appearing. So a published `robots.txt` that names no
sitemap has npanel's `Sitemap:` line appended to it, with everything the site
wrote copied through verbatim. That is a narrow thing to be allowed: `Sitemap`
is a non-group directive, so it grants no crawler anything and cannot change
what an `Allow` or `Disallow` above it means. A file that already names a
sitemap is left alone — it has answered the question, and a second pointer
would be npanel arguing with it — as is one where any group carries a blanket
`Disallow: /`, where an invitation to index would be a strange thing to add on
the site's behalf.

Both kinds of file go into the host's build as blobs of their own, so they are
served like any other file: compressed, with an `ETag` of their own hash, and
with ranges counted against the bytes actually sent. A published file is read
at most 64 KB; anything larger is served as published. The `Sitemap:` line
names the host being served, which is part of why builds are per host — six
sites sharing one boilerplate blob each get their own appended copy.

Page responses carry a `Link: rel="canonical"` header naming the one spelling
of the page, so `/foo`, `/foo/`, `/foo.html` and `/foo/index.html` are indexed
as one page rather than four competing copies. The spelling kept is the clean
one — extensions come off, and `index.html` collapses into its directory —
because that is the URL static site generators actually link to: a VitePress
build with `cleanUrls` publishes `hacking.html`, links to `/hacking`
throughout, names that URL in its own sitemap and puts it in the page's
`og:url`. Naming the file instead had npanel contradicting sites about their
own addresses. It is safe rather than a guess because the clean URL is always
servable here — resolution tries a `.html` sibling for exactly this reason —
and only `.html` is stripped, since `.htm` has no such step. The sitemap uses
the same function, so the two can't disagree. The header is only sent when the
request path actually spells the file it resolved to, which keeps it off
SPA-mode fallbacks: a shell answering for `/any/client/route` with a canonical
of `/` would tell crawlers the whole app is one page.

All three hang off the `seo` flag, per host and defaulting on like `csp`. The
off switch exists because the canonical header is an assertion: a site that
declares `<link rel="canonical">` tags of its own — hreflang sets, staging
mirrors — would be contradicted by it, and npanel doesn't parse every document
to check.

Caching splits by what a file is. A site's URLs are stable across deploys while
the blobs behind them are not, so documents are sent `no-cache` and revalidate
against the `ETag` on every request; assets, which a revalidated document links
to by name, get an hour. Nothing is cached long enough to outlive a deploy.

Nothing is proxied at request time. Every byte comes from local disk, which is
why a served site survives its relays and Blossom servers going offline.

## NIP-05 names

A host can also answer for a set of NIP-05 names, kept in `nip05_names` and
served at `/.well-known/nostr.json`. Staff add them on the site's own tab in the
dashboard, and anyone with a grant on the site can, because a name is a setting
of that site rather than of the instance.

The rule at the path is simple: if the host has names, npanel serves them and
the site's own `nostr.json` is shadowed; if it has none, the path resolves like
any other, serving the deployed file or a `404`. Removing the last name hands
the path straight back. That way round because a `nostr.json` in a repository is
a file every deploy has to remember to carry, and forgetting it quietly unnames
everyone in it — whereas the database survives a deploy that knows nothing about
it.

The names are answered before the deploy check, so one resolves the moment it is
added, whether or not the site behind the hostname has ever finished deploying.
They are the gateway's data, not the manifest's. A disabled host still answers
for nobody, since that hostname isn't served at all.

A request naming a name gets that one, and a request naming none gets the whole
list — which is what lets other tools discover who a domain hosts, and is the
same list a static file would have handed over. A name the host doesn't have is
an empty document under a `200` rather than a `404`: the question was answered.
Every site response is already `Access-Control-Allow-Origin: *`, which NIP-05
requires, because a browser-only client cannot tell a CORS refusal apart from a
missing file.

Names live in a table of their own keyed by host id rather than hostname, so
renaming a site carries them along and deleting it takes them with it — the
same `ON DELETE CASCADE` reasoning as the onion keys. The next site to be given
that hostname never made those assertions about anyone.

## Environment variables

A host can also carry variables, which its pages read as `window.ENV`. Staff
set them on the site's Environment page, a name and a value to a row, and as
with NIP-05 names anyone with a grant on the site can.

A static site has no server to read configuration from, so the same build goes
everywhere with whatever it was built with. This is the browser half of the
pattern Remix documents: the server — here, the gateway — renders the values
into a script that runs before the site's own, and a site written to look
reads `window.ENV.API_URL` rather than a constant baked in at build time. One
deploy can then be served on two hosts pointed at two backends, and a backend
can move without a rebuild. A site that doesn't look is unaffected.

The script is part of the host's build (`src/site_env.rs`, `src/builds.rs`),
made as the tracker is: stored as a blob at `/_npanel/env.js`, wrapped in a
LibreJS license block, and loaded by every page, which is rewritten once to
say so. The differences are all about running first. The tag goes at the
*start* of the head rather than the end, and is a classic script rather than a
module, so it blocks the parser and has set `window.ENV` before any script the
site wrote starts. It also comes before any `<base>` that could send its path
somewhere else. A `<meta>` Content Security Policy after it is widened all the
same, since npanel sends a declared policy as a header, and a header covers
the whole page.

The script assigns `self.ENV`, which on a page is `window.ENV`. In a worker
there is no `window` but there is a `self`, so a site's service worker can
`importScripts` the same file and read the same variables.

The tag loads the script at a versioned URL, like every script npanel adds
(see [builds](#builds)), so a changed value reaches visitors at once.

Blocking the parser has a price: a browser puts no timeout on a script, so a
request that hangs — on a connection that drops packets rather than failing —
leaves the page blank for as long as it does. So the script is served with
`max-age=0, stale-while-revalidate` for a year: a browser that has a copy runs
it at once and refreshes it in the background, and never waits on the network
for it. Only a first visit, or one after the browser evicted it, fetches the
script before the page goes on, over the connection that has just delivered
the page.

A page from the network loses nothing by this. It names the current version,
and a changed value is a new URL nothing has cached, fetched there and then.
The difference is for a site with an offline shell, whose service worker hands
back a page naming the version it was installed with: that page runs the copy
it has, and sees a change one load later — the same lag the shell's own code
has when it updates. Offline, it runs the last values it saw.

Chrome and Edge 75, Firefox 68 and Safari 14 understand the directive. A
browser that doesn't treats the script as `no-cache`, and waits on it every
load as it waits on the page itself.

The variables are part of the build's fingerprint, so changing one rebuilds the
host and nothing else. They enter the fingerprint only when there are some, so
a host without them hashes exactly as it did before variables existed.

Everything in the script is public. It is served to every visitor, and nothing
secret belongs in it — the dashboard says so beside the list.

Variables live in a table keyed by host id, like NIP-05 names, so a rename
carries them along and deleting the host takes them with it.

## Tor

With `tor_enabled` on, every enabled site is also published as a v3 onion
service. Staff do nothing: adding a site mints an address for it, removing one
takes its address away with it.

**Tor is built in.** npanel embeds Arti, the Tor Project's implementation in
Rust, so there is no daemon to install, no control
port to secure, and no second process to keep in step with this one. It runs on
a Tokio runtime and a thread of its own (`src/tor.rs`), restarted like the
deploy runtime if it dies: building circuits, the directory work Arti does
whenever a new consensus arrives, and serving the sites' onion traffic all
happen there, never on the threads serving clearnet visitors.

**npanel keeps the keys and Tor keeps nothing.** Arti's keystore is its
in-memory one, and each service is launched with the key stored in
`onion_services`. The key is what an address is made of, so storing it is what
makes the address survive a restart. What Arti does keep — under
`DATA_DIR/tor`, its directory cache and its guards — makes coming back faster
and keeps the same guards, which is safer; losing it costs a slower start and
no address. Arti also records each service's introduction points, but the keys
those were made with lived in memory and are gone after a restart, so that
record is cleared as Tor starts and every service picks fresh ones, as a C Tor
service that isn't detached does. Each restart therefore republishes every
descriptor, and a visitor holding an old one fetches the new one on their own.

Arti's own logs, which go through `tracing`, are forwarded to npanel's as
`tor_log` entries: its warnings always, its info lines at `LOG_LEVEL=debug`. Keys are stored as C Tor spells
them in `ADD_ONION` (`ED25519-V3:` and the expanded secret key), which is how
they were stored when npanel drove a separate Tor daemon, so every address from
then carried over unchanged.

Arti finds a service's key and its state by a nickname, and each service is
nicknamed after its own address. A nickname can only ever mean one key, so a
host deleted and another created in its place can never inherit its state.
After launching, npanel checks that Arti is answering at the address it was
asked for, because a service launched without its key would quietly make up a
new one.

Sites are *reconciled*, not tracked. Tor starting, adding or removing a host,
and changing a setting all run the same routine: ask the database which hosts
are enabled, and make the running services match. Nothing is remembered between
two runs, so a reconcile interrupted halfway is repaired by the next one, and a
service that fails to launch is retried with backoff.

**Onion connections never reach a listener, and are served as the onion they
arrived at.** Arti hands npanel each stream directly, and it goes to hyper
without touching a socket (`Connections` in `src/server.rs`), so there is no
port for anything else to reach. Tor forwards a visitor's bytes unaltered, so
every header in an onion request is the visitor's to choose, including `Host`.
A stream belongs to one service, so a request whose `Host` names anything but
that service's own address — the dashboard, another site, another onion — gets
a 404. The dashboard and the JSON API cannot be reached through an onion however
a request dresses itself up.

Each request states its `RequestOrigin` outright — `scheme: "http"`, because
there is no proxy and no TLS anywhere on this path — rather than reading
`X-Forwarded-Proto`, which here would be a header the visitor wrote. Tor
authenticates and encrypts the connection itself, which is what the scheme
would otherwise be claiming.

Only a `BEGIN` for port 80 is accepted, and anything else is refused with
`DONE`, as C Tor does, so that nothing about a refusal tells these services
apart from any other.

One consequence elsewhere: `.onion` is in the Public Suffix List, so an onion
address would otherwise read as a root domain and be offered a `www` redirect
into nothing — `isRootDomain` excludes them.

Clearnet HTML responses carry `Onion-Location`, which is how Tor Browser offers
the address to a visitor without anyone having to hand it out. It is sent only
over HTTPS, because on a plain-HTTP hop anything in the path could rewrite it
and point the visitor at an onion of its own choosing.

## Analytics

A site with `analytics` on counts its visitors, and npanel is the analytics
server: it implements the part of Plausible's that a site talks to and the part
its dashboard reads, rather than sending anything anywhere else. There is no
Plausible to install, no site to register in one, and nothing for a site's
author to add — turning the switch on is the whole setup.

**Collection.** The host's build adds Plausible's own tracker
(`@plausible-analytics/tracker`, unmodified) to every page, served from the
site at `/_npanel/script.js` with the site's name baked in, reporting to
`/_npanel/api/event` on the same origin. Same-origin is what keeps it inside
the site's Content Security Policy and working over the site's onion address;
the path is neutral because `plausible` and `analytics` are exactly what
blocklists look for. See [the builds section](#builds) — the pages are rewritten
once, when the host is built, and served like every other stored file.

The tag is an `async` module, so it runs whenever it arrives and nothing waits
on it. A plain module would be deferred, and deferred scripts run in document
order before `DOMContentLoaded`: on a connection that drops packets rather
than failing, the request for the tracker would never end, and every deferred
script after it — the site's own entry point, often — would never start. That
is most likely exactly when it hurts, in a site's offline shell.

**Isolation.** Events are recorded on a runtime of their own, on a thread of
its own (`src/analytics/mod.rs`). Serving sites is always the first priority,
so nothing about counting visits runs on the threads that serve them: not the
parsing, not the user-agent and location lookups, not the writes, and not the
dashboard's queries. The listener that receives an event — the public one, or
Tor's — reads its body, capped at 64 KiB, and hands it across whole. The
runtime owns its database file and its caches outright; if it dies, another is
started with backoff, and sites go on being served either way.

**Ingestion** follows Plausible's closely enough that anything written against
its `POST /api/event` works here: the same fields and limits, the same
`202 ok`, `x-plausible-dropped` for events it chose not to keep, and the same
`400` bodies (`src/analytics/request.rs`, `ingest.rs`). Each event is matched to
a site by its `d`, and dropped with a `202` if that site isn't counting, if the
user agent is a bot, or if the referrer is on Matomo's spam list. Browsers and
operating systems are read with the same Matomo database Plausible uses;
sources and channels are ports of Plausible's rules over its own data files;
locations come from DB-IP's free City Lite database, downloaded into
`DATA_DIR/geo` the first time a site counts visits and refreshed monthly.

**Privacy** is Plausible's model. No cookie is set and nothing identifying is
stored. A visitor is a 48-bit hash of their user agent, address and the site
under a salt that is replaced every day at midnight UTC and deleted the day
after, so one person is one visitor for one day on one site and cannot be
linked further than that by anyone, npanel included. The address is used for
that hash and the location lookup, and dropped.

**Sessions** are Plausible's too: a visit lasts until thirty minutes pass with
no event, takes its source, location and device from the event that began it,
and stops being a bounce on a second pageview or any interactive event.
Engagement events — time on page and scroll depth — keep a visit alive without
counting as anything in it. Open visits live in memory, where each event is
matched to one, and are written back as they change.

**Storage** is SQLite, in `DATA_DIR/analytics.sqlite3`, apart from npanel's own
database so the tables that grow with every pageview are nowhere near the ones
serving reads. Nothing is written per event: events collect in memory and are
written with every changed visit in one transaction every two seconds, so a
crash costs at most those seconds — for counting visitors, a fair price for
never making a visitor's request wait on a disk. Both tables are clustered by
site and time, because every question the dashboard asks is one site over one
range of dates. Every in-memory structure is bounded — open visits, the write
buffer, the user-agent cache — so a flood of events costs accuracy rather than
memory.

**Queries** answer the dashboard's API with Plausible's semantics, not its SQL
(`src/analytics/query/`). Periods, comparisons and time buckets are ports of
Plausible's own functions, so the same query covers the same seconds. SQLite
has no time zones, so bucketing moves each timestamp onto local wall time by
the zone's offset — a `CASE` over the offsets in force across the range, found
from the IANA data compiled into npanel, since a range can straddle a daylight-saving change — before
formatting it the way the labels are. A question becomes at most a few grouped
queries, one over events and one over visits, merged by their dimensions and
ordered and paginated in Rust. Visitors are counted from pageviews when
the question is about pages and from visits otherwise, and a visit-level metric
broken down by page is about the visits that entered there, as in Plausible.
Visits by the hour or minute are counted in every bucket they were active in.

Events reach their visit by id, through an explicitly ordered join: SQLite's
planner, left to itself, starts from the visits and scans a site's events once
for each. On a site of ten thousand visits a day a month's report takes a few
hundred milliseconds to a few seconds, all of it on the analytics thread.

## Link previews

A Nostr web client is usually a static app: one `index.html` that reads the
path — `/npub1…`, `/nevent1…`, `/naddr1…` — and fetches the event from relays
in the browser. A crawler doesn't run it, so every link to a post unfurls as
the app's own name, and search engines index the same empty shell under every
URL. A site with `link_previews` on that publishes a preview script has those
pages answered for crawlers with whatever its script makes of them
(`src/link_previews/`). npanel has no idea of its own what a page about a Nostr
event should say: the site does, and a site without a script gets nothing.

**What qualifies.** A `GET` for a page the script's `routes` name — every page,
if it names none — where the site's answer is an HTML page under a `200`, from
a User-Agent matching `link_preview_crawler_pattern`, on a site with
`link_previews` on and a script. With the `spa` flag that is the shell; without
it, a site that publishes a page at that path. A path with an `nsec` in any
segment never qualifies: running a script for it would put a key in a cache.
Everything else is served exactly as it would be, and every response on such a
page carries `Vary: User-Agent` — with whatever request headers the script
reads — so a shared cache doesn't hand one audience's page to the other.

**What a crawler gets.** The site's own page, with what its head says about the
page — its title, description, Open Graph and Twitter tags, canonical link —
replaced by what the script returned, and its article at the top of the body.
The page is otherwise as published, so it is still the site's — its styles, its
icon, its scripts. A preview with no title is titled as the site — the page's
`og:site_name`, or its title up to the first separator — and one with no image
shows the site's own `og:image`. A page the script has nothing for, returning
`null` or throwing, is served as published.

The article is visible, not hidden. A search engine reads text it can see, and
treats text it can't as an attempt to deceive it. It goes before whatever the
app renders into, so a crawler that runs scripts sees both.

### The script

A site publishes one self-contained module at
`/.well-known/npanel/preview.js` (`src/link_previews/script/`), served like any
other file there: it's public on Blossom anyway. It imports nothing — a static
`import` or `import()` alike fails — so what it uses is bundled into it, and
npanel knows nothing of any other file. A deploy loads the script as a run
would and says in the version's deploy log that it found it, or why it doesn't
load; a script that fails leaves the site without previews, and the build goes
on. Serving loads it the same way, once per build per process, to read its
routes; its top level has two seconds.

```js
export default {
  // URLPattern pathnames; a group may carry its own regular expression.
  routes: ["/:id(n(?:pub|profile|ote|event|addr)1[02-9ac-hj-np-z]+){/}?"],
  async preview(request, { nostr, signal }) {
    const pubkey = pubkeyOf(new URL(request.url).pathname);
    const [profile] = await nostr.query([{ kinds: [0], authors: [pubkey], limit: 1 }], { signal });
    if (!profile) return null;
    return { title: "…", description: "…", body: "<article>…</article>", image: draw(profile) };
  },
};
```

**Which pages it previews.** `routes`, when there is one, is a list of up to
32 [URLPattern](https://urlpattern.spec.whatwg.org/) pathnames. A named group
may carry a regular expression, as `:id(n…)` above, so a route can name what
it is about and not every path of its shape. The expressions are Rust's
`regex`, which matches in linear time, held to the crawler pattern's size
limit; they're the same as JavaScript's for anything a path needs, but have no
lookaround or backreferences. They're matched against the path as the crawler
sent it, percent-encoding and all. Routes that don't compile fail the deploy check, and
the site has no previews: a mistake there doesn't become every page.

**What it's given.** The crawler's request, a `GET` without a body, as a
browser's is — its method, its headers, the page's URL without the query a
crawler adds (one page, one cached preview), and as its `signal` the run's
deadline — and an object shaped like Nostrify's `NRelay`: `query(filters)`,
`req(filters)` (the stored events, then `EOSE`), and an `event()` that
refuses, since a preview publishes nothing. The script decodes the path
itself, NIP-05 names included; npanel reads only the relay hints in its NIP-19
segments, to ask besides the configured relays. Credentials and what the proxy
says about the visitor (`Cookie`, `Authorization`, `X-Forwarded-*`, …) aren't
passed on. Copying the request — `clone()`, `new Request(request)` — doesn't
count as reading its headers.

**What it returns.** Any of:

| Field | What npanel writes |
|---|---|
| `title` | `<title>`, `og:title`, `twitter:title` |
| `description` | `description`, `og:description`, `twitter:description` |
| `body` | the `<article>` a crawler reads, as markup, cleaned by an HTML sanitizer of anything that runs |
| `image` | `og:image`, `twitter:image`: a URL, taken relative to the page; a PNG, JPEG, GIF or WebP as a `Uint8Array`, `ArrayBuffer` or `Blob`; or a promise of one |
| `type` | `og:type`, `website` when it's missing |
| `twitter` | `twitter:card`: `summary` for an image shown small, `summary_large_image` (the default) for one shown large |
| `video`, `audio` | `og:video`, `og:audio`, from `{ url, type, width, height }` |
| `published` | `article:published_time`, from seconds or a `Date` |
| `jsonLd` | a schema.org object, written so no value can close its script element |

Or `null`, for nothing. npanel writes only what's returned, and never a guess.

**The image is drawn behind the page.** A promise isn't waited for: the page
goes out as soon as the handler returns, naming its image as
`/_npanel/preview/<the page's path>?v=…`, and the drawing finishes on its own.
The version is a hash of everything the handler was given until it returned:
the script, the URL, the request headers it read, and the filters and URLs it
asked for with what they found. So an image never changes under its name, one
at its version is cached for a week, and a platform that caches images by URL
fetches a new one when anything it's drawn from changes. Anything fetched only
after the handler returns doesn't count, so what the image depends on should
be fetched before. Any client may fetch the image: the services that fetch a
preview's image seldom say they're the crawler that read the page. One asked
for at another version, or none, is the page's current one, for five minutes,
drawn again if it was let go since.

**The web it runs in** is a browser's, as far as a preview needs (`prelude.js`):
`OffscreenCanvas` and its 2D context — paths, `clip`, `drawImage`,
`getImageData` and `putImageData`, gradients, composite operations, `fillText`
and `measureText` — `createImageBitmap` of a PNG, JPEG, GIF, WebP or SVG,
`convertToBlob` to PNG or JPEG, `fetch`, `Blob`, `Headers`, `Request`,
`Response`, `URL`, `TextEncoder`, `atob`, `AbortSignal`, `setTimeout` and
`console`, whose lines go to npanel's log at `debug`. Shadows, filters,
patterns and conic gradients throw rather than draw something wrong, as does
anything else that isn't there. An SVG drawn as an image draws nothing it names
elsewhere, as in a browser: a script fetches what it draws. `Date` stands still
at when the run started, and `Math.random` is seeded by the URL, so one input
gives one image.

**Text is drawn in the gateway's fonts**, the ones installed on its system,
read once when the previews runtime starts; a site ships none. `serif`,
`sans-serif` and `monospace` are what fontconfig says they are, or else DejaVu,
Noto or Liberation, whichever is installed. A character the font asked for
lacks — an emoji, usually — is drawn in one that has it, as a browser does. A
script names a font by its family, and can't reach a file. The Docker image
installs DejaVu and Noto Color Emoji, and the deployment guide has a systemd
install do the same, and says how to add others. Fonts installed later are
read on the next restart, and since an image's version doesn't name the fonts,
images already drawn stay as they were until they expire or the cache is
cleared.

### Limits

**Escaping is the security boundary.** Anyone can send a crawler's User-Agent,
so the page is served on the site's origin to whoever asks, and what a script
writes is made of what anyone can publish to a relay. So the article is
sanitized, every other field is escaped where it's written, URLs are kept only
when they're `http(s)`, and the JSON-LD escapes `<`, `>` and `&`. The page goes
out under the site's Content Security Policy like any other, and nothing npanel
adds to it is a script that runs.

**A script is held to npanel's limits.** Each run gets a QuickJS runtime of its
own on a thread of its own, as many at once as there are cores: 64 MB of memory,
ten seconds to return and ten more to draw, past which it is interrupted
wherever it is. It may query relays eight times, for fifty events in all — each
filter naming ids, authors or a tag, and its `limit` honoured however many a
relay sends. It may fetch sixteen `https://` URLs, through the
public-addresses-only resolver blob downloads use, following redirects only to
`https://`, 8 MB each and 32 MB in all; it reads no file. Its canvases and
decoded images may hold 16 million pixels between them, none over 2400 pixels a
side for a canvas or 4096 for a picture. The script may be 4 MB.

**A run starts in a few milliseconds.** A fresh runtime costs well under one;
what costs is the code. The prelude and a script are compiled once — the
prelude by the process's first run, a build's script when it's first loaded —
and later runs load their QuickJS bytecode, which takes a tenth of the time
parsing does. What a script does at its top level still runs every time, so a
script should bundle only what it uses: Ditto's bundle once spent a tenth of a
second building AES tables for ciphers its previews never call.
`convertToBlob` encodes after the handler has had the chance to return, so a
handler that returns its image as a promise sends the page without waiting
for the encoding, the slowest part of drawing. `bench_preview`
(`docs/development.md`) times each part.

**Serving never waits long.** Scripts run on a runtime and a thread of their
own, with a relay pool of their own, restarted with backoff like the analytics
runtime. A request asks memory first; a preview there is sent at once, and a
stale one is sent while a fresh one is made behind it. Otherwise it hands the
lookup across and waits two and a half seconds at most. A lookup that can't
start — `link_preview_concurrency` are running, or the runtime is starting — or
doesn't finish in time sends the page as published. Neither is ever an error:
the worst a visitor sees is the site. A lookup that outlives its request goes
on, and fills the cache for the next.

**Relays are spared too.** A crawl asks for hundreds of pages at once, and
npanel's deploys depend on the same relays, so scripts' queries that arrive
within fifty milliseconds of each other share one subscription per relay, and
are sorted out from the answer: identical filters are sent once, and filters
for authors' newest replaceable or addressable events of the same kinds become
one filter. Every lookup of one page at once is the same lookup. Runs are
metered by `link_preview_misses_per_minute`; cached previews never count, so
the meter is exactly what a stranger requesting made-up ids can cost. A lookup
over it is answered from whatever is cached. Queries go to the configured
relays and the path's own hints and, when those have nothing, to the outboxes
of the authors the filters name. Every relay an author names is held to the
rule deploys follow: `wss://`, public addresses, five at most.

**Storage.** Previews are kept in memory, up to 32 MB of them, and in
`DATA_DIR/previews.sqlite3` for `link_preview_ttl` seconds — a file apart from
the gateway's database, so clearing it and `VACUUM`ing it touch nothing else,
and what it takes up is its size. It's a cache, so its layout is versioned as a
whole, and one an older npanel left is thrown away. A preview is kept by the
script, the URL, and the values of the request headers the script read, which
are remembered too; two builds with the same script share them. A script having
nothing for a page is remembered for ten minutes at most, since it may only
mean the relays were slow; for the same reason, a run that has nothing where it
had something keeps what it had and tries again in ten minutes, as does a run
that fails. Past `link_preview_max_entries`, the least recently made rows go.
Drawn images are kept by version, 64 MB of them in memory and 512 MB in
`DATA_DIR/preview-images/`.

**Settings → Link Previews** shows what is cached and how lookups have gone,
runs one page's script again from its URL, and clears the lot, images
included.

## Authentication

NIP-98: the client signs a kind 27235 event naming the method and URL, and sends
it base64-encoded in `Authorization: Nostr <event>`. npanel verifies the
signature, checks the event is less than 60 seconds old, that the method
matches, and — for requests with a body — that it carries a `payload` tag
matching the body's sha256. The tag is required rather than checked only when
present: it is the one thing tying a body to the signature, and without it a
header seen once could be replayed within the minute with any body at all.

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

`Settings` (`src/settings.rs`) caches settings in memory and publishes each new
set of values on a watch channel. A subscriber compares the fields it cares
about with the last values it saw, so one that falls behind skips to the
newest values rather than missing a change, and a field it names is one the
compiler has checked exists. Subscribers reconfigure in place: the relay pool clears its list caches, the
subscription restarts — so a relay that is gone is asked nothing more and its
connection closes itself once idle — the resync
timer re-arms, and adding Blossom fallbacks or raising the size limit
immediately retries every version that isn't ready — because that's often
exactly what a failed deploy was waiting for.

The deploy, analytics and Tor runtimes read the same settings and receive the same
change notices, so their subscribers reconfigure exactly as the rest do. The previews runtime reads them afresh for every lookup, so a
change applies to the next one with nothing to reconfigure.

Nothing in the settings UI requires a restart, which is the point: the six
environment variables are the only things that do.
