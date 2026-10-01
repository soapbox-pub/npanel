# Architecture

npanel is one process that does three things: it serves nsites over HTTP, it
serves a dashboard for configuring which nsites those are, and, on threads of
its own, it keeps local storage in sync with Nostr.

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

   /_npanel/api/event on any site ──► analytics thread ──► analytics.sqlite3
```

## Request routing

Everything hangs off the `Host` header, checked in one middleware in
`src/app.rs`. If it equals `DASHBOARD_HOST`, the request goes to the axum API at
`/api` and otherwise to the built dashboard SPA. Any other hostname is served as
an nsite.

Only `Host` is read. Ports and the trailing dot of an FQDN are stripped and the
name is lowercased (`request_hostname`, `src/hostname.rs`, shared with the Tor
listener so the two cannot drift). `X-Forwarded-Host` is deliberately ignored:
npanel cannot tell a proxy's copy of that header from a client's, and a proxy
that doesn't set one — nginx doesn't — would pass a visitor's straight through,
letting a request for any site ask for the dashboard instead. A proxy that
rewrites `Host` has to be configured not to; see
[deployment](deployment.md#other-proxies).

The scheme is a different matter, and comes from `X-Forwarded-Proto`
(`request_scheme`, `src/app.rs`), defaulting to `https`. npanel terminates no
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

The same build is also published as an nsite, where nothing serves the API on
its origin. It finds its server at startup (`dashboard/src/lib/server.ts`): if
`/api/instance` on its own origin answers as npanel, that is the server;
otherwise it asks the reader for one, and remembers it. Every API response
allows any origin, and NIP-98 binds a request to the API's URL rather than the
page's, so a dashboard on another origin needs nothing else from the server.

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
per manifest. Superseded versions still hold their blobs until `prune-blobs`
removes them, so a gateway serving sites it doesn't trust should run it on a
schedule.

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
prepared statements.

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
other static host does. Requests for anything else keep their `404` in both
modes — a script or stylesheet answered with a page of HTML and a `200` doesn't
render the site, it turns a plain missing file into a parse error somewhere
else. The flag is off for every existing host and every new one: folding `www`
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
practice: `prune-blobs --keep N` forgets old versions first, so the next build
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

The deploy and analytics runtimes read the same settings and receive the same
change notices, so their subscribers reconfigure exactly as the rest do.

Nothing in the settings UI requires a restart, which is the point: the six
environment variables are the only things that do.
