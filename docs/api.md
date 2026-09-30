# API

The dashboard is a client of this API and uses nothing private, so anything the
UI can do, a script can do.

Everything lives under `/api` on `DASHBOARD_HOST`. All responses are JSON,
including `404`s for routes that don't exist. Errors are `{ "error": "..." }`
with a 4xx or 5xx status. Request bodies are limited to 1 MB; anything larger
is `413`, refused before the signature is checked.

## Authentication

Every route except `/api/instance` and `/api/sites` requires [NIP-98](https://github.com/nostr-protocol/nips/blob/master/98.md) auth. Sign a
kind `27235` event and send it base64-encoded:

```
Authorization: Nostr <base64(event)>
```

The event needs:

| Tag       | Value                                                         |
| --------- | ------------------------------------------------------------- |
| `u`       | The full public URL, e.g. `https://panel.example.com/api/hosts` |
| `method`  | The HTTP method, uppercase                                     |
| `payload` | sha256 of the request body — required only when there is one   |

`created_at` must be within 60 seconds of the server's clock. Each event is for
one request; sign a fresh one every time.

The `u` tag is checked against `DASHBOARD_HOST` and the request path, not
against the URL the process sees, so a reverse proxy rewriting the scheme or
port is fine. Host and path must match exactly.

A valid signature from a pubkey that isn't in the `users` table gets `403`, not
`401` — npanel is permissioned, and signing proves who you are, not that you're
allowed in.

Roles:

- **admin** — everything
- **staff** — only hosts explicitly granted to them, plus any host they created

Routes marked **admin** below return `403` for staff.

## Certificates

Certificate authorization for the reverse proxy is **not part of this API**. It
answers on its own port, `TLS_CHECK_PORT`, because the proxy asks with no
hostname npanel could route on — see
[deployment](deployment.md#5-tls-with-caddy).

```
GET http://127.0.0.1:3001/tls-check?domain=<hostname>
```

`200` with `ok` when the domain is `DASHBOARD_HOST` or an enabled host, `404`
otherwise, and `429` with a `Retry-After` once `tls_check_limit_per_host` or
`tls_check_limit_total` is spent. Refusals aren't metered, and `DASHBOARD_HOST`
is exempt. Nothing else is served on that port.

## Instance

### `GET /api/instance`

Unauthenticated, for the logged-out homepage, which introduces npanel to
whoever landed on it and names the person running this copy.

```json
{ "contact_npub": "npub1...", "dashboard_host": "panel.example.com" }
```

`contact_npub` is `null` when no admin has set
[`contact_npub`](configuration.md#contact_npub). Nothing here is private: the
npub is published to be shown, and the hostname is the one the caller used to
reach this route.

### `GET /api/sites`

Unauthenticated. The sites this gateway will admit to serving, for the
logged-out homepage.

```json
{
  "sites": [
    {
      "hostname": "example.org",
      "title": "The Docs",
      "description": "Everything about the thing.",
      "image": "https://example.org/cover.png",
      "icon": "https://example.org/apple-touch-icon.png"
    }
  ]
}
```

`sites` is empty unless an admin turned
[`public_site_list`](configuration.md#public_site_list) on, and never includes a
host that is disabled or marked `hidden`. Nothing here says which nsite a site
points at, or who runs it.

`title`, `description`, and `image` are the site's own Open Graph, Twitter, or
`<title>` metadata, read from the copy of its `index.html` npanel already
serves — no crawling, and no request leaves the box. Each is `null` when the
page doesn't say: a site with nothing deployed yet is still listed, as a bare
hostname. `image` is always an absolute `https` URL, resolved against the site's
own hostname, since a relative `og:image` belongs to the site npanel serves.

`icon` is the site's own icon, picked the way a home screen would: an
`apple-touch-icon` first, then an icon from the web app manifest the page
links (any but a bare `monochrome` silhouette), then a `<link rel="icon">`,
the largest of each kind first. Files at the conventional names —
`/apple-touch-icon.png`, `/icon.svg`, `/favicon.ico` and the like — count even
when nothing links them. An icon on the site's own domain
counts only if the build really contains the file, so a stale link is skipped
rather than shown broken. `null` when there is none.

## Session

### `GET /api/me`

```json
{ "pubkey": "<hex>", "npub": "npub1...", "role": "admin" }
```

## DNS

### `GET /api/dns`

The records that point a hostname at this server, which the dashboard shows
while staff are adding one. Available to any authenticated user.

```json
{
  "records": [
    { "type": "A", "value": "203.0.113.10" },
    { "type": "AAAA", "value": "2001:db8::1" }
  ],
  "source": "dashboard_host",
  "dashboard_host": "panel.example.com"
}
```

`source` is `settings` when an admin set `dns_targets`, and `dashboard_host`
when the answer was detected by resolving `DASHBOARD_HOST` — see
[configuration](configuration.md#dns_targets). `records` is empty when neither
produced anything; `type` is `A`, `AAAA`, or `CNAME`.

## Hosts

A host is addressed by its hostname, which is unique — `/api/hosts/example.org`.
The database's row id is never part of the API.

A host object:

```json
{
  "hostname": "example.org",
  "pubkey": "<hex>",
  "kind": 35128,
  "identifier": "docs",
  "address": "35128:<hex>:docs",
  "naddr": "naddr1...",
  "enabled": true,
  "hidden": false,
  "redirect_www": true,
  "spa": false,
  "csp": true,
  "csp_policy": "default-src 'none'; script-src 'self'; …",
  "csp_source": "gateway",
  "seo": true,
  "merge_deployments": false,
  "analytics": false,
  "onion": "abc…xyz.onion",
  "pinned_event_id": null,
  "created_by": "<hex>",
  "created_at": 1754000000,
  "updated_at": 1754000000,
  "active": { "...": "the version the host's build serves, or null" },
  "latest": { "...": "the newest known version, or null" }
}
```

`onion` is read-only, and `null` unless
[`tor_enabled`](configuration.md#tor_enabled) is on. npanel assigns it, not
staff: enabling Tor gives every enabled host an address, a host added afterwards
gets one a moment after it is created, and deleting a host takes its address
with it for good. It is stable otherwise — a restart of npanel or of Tor
republishes the same address. There is no field to set it and no per-host way to
opt out.

A version object:

```json
{
  "event_id": "<hex>",
  "created_at": 1754000000,
  "status": "ready",
  "files_total": 42,
  "files_ready": 42,
  "error": null,
  "fetched_at": 1754000001,
  "ready_at": 1754000009
}
```

`status` is one of `pending`, `fetching_blobs`, `ready`, `failed`. Only `ready`
versions are ever served, and only once the host has been built from one, so
`active` and `latest` differ while a deploy or the build after it is in flight,
or after either fails. A failed build says why in the version's deploy log.

### `GET /api/hosts`

```json
{ "hosts": [...], "other_sites": [{ "hostname": "example.org" }] }
```

`hosts` is ordered by hostname; staff see only the hosts granted to them. Each
host carries a `preview` — `title`, `description`, `image` and `icon`, exactly
as [`/api/sites`](#get-apisites) describes them — whether or not it is listed.
`other_sites` names what else the gateway serves, on exactly the terms
`/api/sites` does — so it is empty when `public_site_list` is off, and always
empty for an admin, whose `hosts` already holds everything.

### `POST /api/hosts`

```json
{ "hostname": "example.org", "address": "npub1...", "hidden": false, "redirect_www": true, "spa": false, "csp": true, "seo": true, "merge_deployments": false, "analytics": false }
```

`address` accepts an npub, nprofile, naddr, an `a` tag
(`35128:<pubkey>:<identifier>`), a NIP-5A label, or a URL containing one of
those. It's stored decomposed into pubkey, kind, and identifier.

`201` with `{ "host": {...} }`. `409` if the hostname is already configured or
is `DASHBOARD_HOST`. `400` if it doesn't sit under one of
`allowed_host_suffixes`, when that setting is non-empty. A staff user who
creates a host is granted it automatically, so they aren't locked out of their
own work.

### `GET /api/hosts/:hostname`

`{ "host": {...} }`, or `404` / `403`.

### `PATCH /api/hosts/:hostname`

Any subset of:

```json
{
  "hostname": "new.example.org",
  "address": "naddr1...",
  "enabled": false,
  "hidden": true,
  "redirect_www": true,
  "spa": true,
  "csp": false,
  "seo": true,
  "merge_deployments": true,
  "analytics": true,
  "pinned_event_id": "<event id>"
}
```

Set `pinned_event_id` to an event id to freeze the host on that version, or
`null` to follow the newest ready one. Pinning a version that belongs to a
different site is `400`; pinning one that isn't fully downloaded is `409`.

Changing `address` clears any pin, since it belonged to the old site.

`hidden` keeps the site out of `/api/sites` and out of other users'
`other_sites`. It changes nothing about serving it: an unlisted site answers
requests exactly as before.

`redirect_www` sends `www.<hostname>/<path>` to `<hostname>/<path>` with a
`301`, and defaults to on. It only acts when the hostname is a root domain
(`example.org`, `example.co.uk` — decided by the Public Suffix List), and never
when `www.<hostname>` is configured as a host of its own. Setting it on a
subdomain is accepted and does nothing, so a later rename doesn't silently
leave `www` unserved.

`spa` decides what a path the site has no file for returns, and defaults to
off. Off, it's a `404`, carrying the site's own `/404.html` if it published
one. On, a *document* request — a path with no extension, or one ending
`.html` — gets the site's shell (`/404.html` if present, else `/index.html`)
with a `200`, so a client-side router can decide what the page is. Requests for
anything else still `404` either way: a script or stylesheet that answers `200`
with a page of HTML doesn't render the site, it fails somewhere harder to read.

`csp` decides whether this site is served under a `Content-Security-Policy` at
all, and defaults to `true` — including for hosts created before the flag
existed. *Which* policy is the site's own decision, and the read-only
`csp_source` says which of the two it got:

`"site"` — the site declares a policy in a `<meta http-equiv>` tag of its
`/index.html`, and npanel sends that one, verbatim, on every response for the
host. Multiple tags become the comma-separated list the header defines, which is
how a browser reads several meta policies anyway. A `-Report-Only` tag is not a
declaration and is skipped, and so is a policy carrying a newline or one longer
than 8 KB — those fall back to the gateway's.

`"gateway"` — the site declares nothing, so it gets npanel's own:

```
default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline';
frame-src 'self' https:; font-src 'self'; base-uri 'self'; manifest-src 'self';
connect-src 'self' blob: https: wss:; img-src 'self' data: blob: https:;
media-src 'self' https:
```

That is verbatim the policy [mkstack](https://mkstack.xyz) ships as a `<meta>`
tag, which is what most sites reaching an npanel gateway are built from. Sent as
a header it also covers what a meta tag can't: responses that aren't HTML, and
the fact that a document's own `<meta>` is only reached after the parser has
already run whatever came before it.

`csp_policy` is the policy actually in force, whichever it came from, and `null`
when the flag is off. Both fields are computed from the version being served, so
they change when the site redeploys.

Because a declared policy replaces rather than joins the gateway's, a site can
widen its own: publishing `script-src 'unsafe-inline'` in `/index.html` is
enough. That is the site author's call to make about their own hostname — it is
their code running either way — and it reaches no other host. What it does mean
is that `csp: true` is a promise that *a* policy is sent, not that npanel's is.

Turning the flag off sends no policy at all, meta tag or not. It is there for
the site that needs one the gateway's forbids — `script-src 'self'` blocks
inline `<script>` blocks and CDN scripts, and the script silently doesn't run —
and that can't be rebuilt or given a policy of its own. It is scoped to the one
site: there is no instance-wide switch, so turning it off for the site that
needs it can't accidentally disarm the rest.

`seo` decides whether npanel does three things for the site's standing in
search engines, and defaults to `true` — including for hosts created before the
flag existed. A `/sitemap.xml` is generated from the version's file list, and a
`/robots.txt` naming it, each only when the site published no file at that
exact path. And every page response carries a `Link: rel="canonical"` header
naming its one URL, so `/foo`, `/foo/`, `/foo.html` and `/foo/index.html` are
indexed as one page rather than four competing copies. The URL named is the
clean one: `.html` comes off and `index.html` collapses into its directory,
which is both what site generators link to and always servable here, since an
extensionless path resolves to its `.html` sibling. `.htm` is left alone, as
it has no such step. Turn the flag off for a site that manages canonical URLs
itself — one that declares `<link rel="canonical">` tags of its own would be
contradicted by the header.

`robots.txt` is the one file npanel will add to rather than only stand in for.
A site that published one naming no sitemap gets npanel's `Sitemap:` line
appended to it; everything the site wrote is copied through verbatim, never
reflowed or reordered. `Sitemap` is a non-group directive, so the addition
grants no crawler anything and cannot change what an `Allow` or `Disallow`
above it means. The file is left exactly as published when it already names a
sitemap — including one of its own, like a `sitemap-index.xml` — when any
group carries a blanket `Disallow: /`, or when it is larger than 64 KB. The
appended file is made when the host is built and stored like any other, so it
is served compressed, with an ETag of its own hash and range support.

`merge_deployments` stacks the site's earlier deploys underneath the one being
served, oldest first, and defaults to `false`. A path is then answered from the
newest deploy that has it, so the current deploy wins everywhere it published a
file and a path it dropped goes on being served from the deploy that last did.
Only fully-downloaded versions take part, and never one newer than the version
being served — a pinned host merges up to its pin. Turn it on for a build that
loads modules by name at runtime, like Vite: a page a visitor already has open
goes on requesting the chunk filenames of the deploy it was loaded from, and
those are exactly the files the next deploy removes. The generated
`sitemap.xml` is unaffected — it lists the served version's own pages, since
keeping a withdrawn URL answering is not a reason to invite a crawler to index
it.

`analytics` counts visits to the site, and defaults to `false`. With it on,
every HTML page the site serves loads npanel's tracker — Plausible's own
`@plausible-analytics/tracker`, served from the site at `/.npanel/script.js`
and reporting to `/.npanel/api/event` on the same origin — so nothing needs
configuring and a visit over the site's onion address counts too. The pages are
rewritten when the host is built, not per request: the one tag is added at the
end of `<head>`, every other byte is left as published, and the result is
stored and compressed like any other file. A `<meta>` Content Security Policy
in a page is widened just enough for the tracker — `'self'` added to whichever
directives govern loading a script and `fetch`ing — and the header follows
the page. A site that publishes files under `/.npanel/` has
`/.npanel/script.js` shadowed while this is on.

`hostname` is validated exactly as on create: `409` if it's already configured
or is `DASHBOARD_HOST`, `400` if it falls outside `allowed_host_suffixes`. A
rename also moves the host: every subsequent request must use the new name.

A change to `hostname`, `address`, `pinned_event_id`, `seo`,
`merge_deployments`, or `analytics` rebuilds the host in the background, since each changes
the files it serves. The response doesn't wait: the previous build goes on
serving until the new one is complete, which is usually a moment.

### `DELETE /api/hosts/:hostname`

`{ "ok": true }`. Removes the mapping; stored versions and blobs are untouched
until `prune-blobs` runs.

### `GET /api/hosts/:hostname/versions?limit=50`

```json
{
  "active_event_id": "<hex or null>",
  "pinned_event_id": "<hex or null>",
  "versions": [ ... ]
}
```

Newest first. `limit` is clamped to 1–200.

### `GET /api/hosts/:hostname/versions/:eventId/log`

The deploy trace for one version — what the dashboard shows when a deploy fails.

```json
{
  "version": { "...": "version object" },
  "entries": [
    {
      "id": 12,
      "level": "warn",
      "msg": "blob_download_failed",
      "fields": { "sha256": "...", "server": "...", "reason": "http", "status": 404 },
      "created_at": 1754000005
    }
  ]
}
```

Oldest first, capped at the 500 most recent entries per version.

### `GET /api/hosts/:hostname/files?event_id=<hex>`

Files in a version, defaulting to the active one (or the newest if none is
ready).

Without `event_id` this is the host's build — what the site actually serves.
On a host with `merge_deployments` it carries the paths held over from earlier
deploys, each marked `inherited`, and it carries the files npanel generated or
rewrote, marked by `source`. Naming a version with `event_id` lists that
version's own manifest and nothing else.

```json
{
  "event_id": "<hex>",
  "files": [
    {
      "path": "/index.html",
      "sha256": "<hex>",
      "size": 1024,
      "stored": true,
      "inherited": false,
      "source": "site"
    }
  ]
}
```

`stored` is false for a blob that hasn't been downloaded yet — which is how you
spot exactly what a stalled deploy is missing. `inherited` is true for a path
the named version doesn't publish and a merged earlier one does. `source` is
`site` for a file as published, `generated` for one npanel made outright — a
sitemap, the analytics tracker — and `modified` for one it rewrote, like a
`robots.txt` with a line appended or a page loading the tracker.

### `POST /api/hosts/:hostname/sync`

Fetch the newest manifest from relays now. Returns `202` immediately;
relay round trips take seconds, so poll the host or its versions for the result.

### `GET /api/hosts/:hostname/grants` — admin

`{ "grants": [{ "user_pubkey": "<hex>", "created_at": 1754000000 }] }`

### `PUT /api/hosts/:hostname/grants` — admin

```json
{ "pubkeys": ["<hex>", "<hex>"] }
```

Replaces the grant list wholesale. Every pubkey must be hex and must already be
an npanel user. Returns `{ "grants": [...] }`.

### `GET /api/hosts/:hostname/nip05`

The NIP-05 names this host answers for, ordered by name.

```json
{
  "names": [
    {
      "name": "bob",
      "pubkey": "<hex>",
      "relays": ["wss://relay.example.com"],
      "created_at": 1754000000,
      "updated_at": 1754000000
    }
  ]
}
```

Not admin-only. A name is a setting of the site, so anyone with a grant on the
site manages its names.

### `PUT /api/hosts/:hostname/nip05/:name`

```json
{ "pubkey": "npub1… | nprofile1… | <hex>", "relays": ["wss://relay.example.com"] }
```

Adds the name, or replaces whatever was stored under it — `201` for the first,
`200` for a replacement. Either way the response is the whole list, in the shape
`GET` returns.

`name` is the local part, lowercased: the `bob` in `bob@example.com`. NIP-05
restricts it to `a-z0-9-_.`, and anything else is a `400` rather than something
escaped, since a name outside that set is one no client will ever look up. `_`
is legal and is the identifier clients render as the bare domain.

`relays` is optional, at most ten, and each must be `wss://`. They
are normalized and deduplicated, so the same relay written two ways is stored
once. The pubkey is stored and served as hex whatever form it arrives in — the
NIP forbids `npub` in the document itself.

### `DELETE /api/hosts/:hostname/nip05/:name`

Removes one name; `404` if the host never had it. Returns the remaining list.
Deleting the host deletes its names with it.

## Analytics

A site's analytics, for anyone who may manage the site. These are the three
endpoints Plausible's own dashboard reads — `POST /api/stats/:domain/query`,
`current-visitors` and `suggestions` — under the host instead of a Plausible
domain, with the same bodies in and out, so anything written against
Plausible's dashboard API reads npanel's. Site and user management have no
counterpart: the grant npanel already checks is the whole of access control.

They answer whether or not the site is counting visits right now, since the
numbers it collected while it was are still there. While the analytics thread
is starting or restarting, every one of them is a `503`.

### `POST /api/hosts/:hostname/stats/query`

Plausible's dashboard query. The body:

```json
{
  "date_range": "28d",
  "relative_date": "2026-09-28",
  "metrics": ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"],
  "dimensions": [],
  "filters": [["is", "visit:source", ["Google"]]],
  "order_by": [["visitors", "desc"]],
  "pagination": { "limit": 9, "offset": 0 },
  "include": {
    "time_labels": false,
    "partial_time_labels": false,
    "compare": "previous_period",
    "compare_match_day_of_week": true,
    "empty_metrics": false,
    "present_index": false
  }
}
```

- `date_range` is `realtime` (the last five minutes), `realtime_30m`, `day`,
  `24h`, `7d`, `28d`, `30d`, `91d`, `month`, `6mo`, `12mo`, `year`, `all`, or
  `["YYYY-MM-DD", "YYYY-MM-DD"]`. `Nd` is the N full days before
  `relative_date`, and `Nmo` the N full months before its month, as in
  Plausible; `month`, `year` and `day` in progress end now. Days are days in
  [`analytics_timezone`](configuration.md#analytics_timezone).
- `metrics`: `visitors`, `visits`, `pageviews`, `views_per_visit`,
  `bounce_rate`, `visit_duration`, `time_on_page` and `scroll_depth` (with a
  page filter or dimension), `percentage`, and `exit_rate` (with the
  `visit:exit_page` dimension).
- `dimensions`: `time:minute|hour|day|week|month`, `event:page`, and
  `visit:source`, `referrer`, `channel`, `utm_medium`, `utm_source`,
  `utm_campaign`, `utm_content`, `utm_term`, `entry_page`, `exit_page`,
  `country`, `country_name`, `region`, `region_name`, `city`, `city_name`,
  `device`, `browser`, `browser_version`, `os`, `os_version`.
- `filters`: `[operator, dimension, [values], {"case_sensitive": false}?]`,
  with `is`, `is_not`, `contains` and `contains_not`, on `event:page` or any
  `visit:` dimension.
- `include.compare`: `previous_period`, `year_over_year`, or a pair of dates.

The response is Plausible's: `results` as `{dimensions, metrics}` rows in the
order asked for, `comparison` on each row or — for a time series —
`comparison_results` beside them, `meta` with the time labels and their
indices, the present and partial buckets and the empty metrics that were asked
for, and the `query` echoed with its resolved date ranges.

What differs is only what npanel doesn't record: there are no goals, custom
properties, revenue, imports or segments, and a query asking for them is a
`400`. Regions and cities are identified by name — `DE-Bavaria`,
`DE-Bavaria-Munich` — rather than by ISO code and GeoNames id, because DB-IP's
free database carries neither.

A query Plausible would refuse is a `400` with `{"error": "…"}` saying why.

### `GET /api/hosts/:hostname/stats/current-visitors`

The number of visitors seen in the last five minutes, as a bare JSON number.

### `GET /api/hosts/:hostname/stats/suggestions/:filter`

Values the dashboard's filter box can offer for `:filter` — `page`,
`entry_page`, `exit_page`, `source`, `channel`, `referrer`, `utm_*`,
`country`, `region`, `city`, `screen`, `browser`, `browser_version`, `os`,
`os_version` — most visited first, at most 25. The query string is Plausible's
older form: `period`, `date`, `from` and `to` for the range, `filters` as a
JSON list of filters as above, and `q` for what has been typed.

```json
[{ "value": "DE", "label": "Germany" }, { "value": "US", "label": "United States" }]
```

### `GET /api/stats/sparklines`

The last 24 hours of every site the caller may manage, for the dashboard's
list of sites: unique visitors, their percent change against the 24 hours
before (100 when there were none), and visitors in each hour, oldest first —
25 buckets, the first and last partial.

```json
{ "sparklines": { "docs.example.com": { "visitors": 42, "change": -8, "hours": [0, 3, 1, "…"] } } }
```

## Users — admin

### `GET /api/users`

```json
{
  "users": [
    {
      "pubkey": "<hex>",
      "npub": "npub1...",
      "role": "staff",
      "added_by": "<hex>",
      "created_at": 1754000000,
      "hostnames": ["example.org", "docs.example.org"]
    }
  ]
}
```

### `POST /api/users`

```json
{ "pubkey": "npub1...", "role": "staff" }
```

`role` defaults to `staff`. Accepts npub, nprofile, or hex. `201`, or `409` if
they already have access.

### `PATCH /api/users/:pubkey`

```json
{ "role": "admin" }
```

### `DELETE /api/users/:pubkey`

Removes the user and their grants.

Demoting or deleting the last admin is `409` — there is no way to lock every
admin out of the dashboard through the API. (`npanel make-admin` is the way
back in if it ever happens anyway.)

## Settings — admin

### `GET /api/settings`

```json
{
  "settings": {
    "relays": ["wss://relay.ditto.pub"],
    "blossom_fallback_servers": ["https://blossom.ditto.pub/"],
    "max_file_size": 104857600,
    "max_site_size": 1073741824,
    "max_site_files": 50000,
    "deploy_attempts": 3,
    "resync_interval": 600,
    "dns_targets": [],
    "allowed_host_suffixes": [],
    "tls_check_limit_per_host": 60,
    "tls_check_limit_total": 600,
    "contact_npub": "",
    "public_site_list": false
  },
  "defaults": { "...": "the built-in defaults, for a reset button" }
}
```

### `PUT /api/settings`

Send any subset of the keys. Unknown keys are `400`, as is any value that fails
validation, with a message naming the offending key.

```json
{ "settings": { "...": "the new values" }, "changed": ["relays"] }
```

Changes apply immediately — no restart — and each changed key is written to the
audit log. See [configuration](configuration.md#runtime-settings) for what each
key does.

## Audit log — admin

### `GET /api/audit-log?limit=100&offset=0`

```json
{
  "entries": [
    {
      "id": 41,
      "actor_pubkey": "<hex>",
      "action": "host.update",
      "target": "example.org",
      "old_value": { "...": "before" },
      "new_value": { "...": "after" },
      "created_at": 1754000000
    }
  ]
}
```

Newest first. `limit` is clamped to 1–500. Actions are `host.create`,
`host.update`, `host.delete`, `host.grants`, `user.create`, `user.update`,
`user.delete`, and `settings.update`.

## Status

### `GET /api/status`

```json
{
  "hosts": 12,
  "ready_versions": 30,
  "blobs": 1840,
  "blob_bytes": 419430400,
  "relays": ["wss://relay.ditto.pub"],
  "tor": {
    "enabled": true,
    "connected": true,
    "services": 12,
    "error": ""
  }
}
```

Available to any authenticated user.

`tor.enabled` is the setting; `connected` is whether npanel currently has a
control connection, and `services` how many onion addresses are published over
it. `error` is empty while connected and otherwise says why not, as a sentence —
npanel keeps retrying with backoff, so a false `connected` is a live condition
rather than a permanent one. See [Tor](deployment.md#tor).

## Site responses

Not part of the JSON API, but worth knowing when debugging a served site:

| Header                    | Meaning                                          |
| ------------------------- | ------------------------------------------------ |
| `ETag`                    | The blob's sha256, quoted, suffixed per encoding |
| `Last-Modified`           | The manifest event's `created_at`                |
| `Content-Encoding`        | `br` or `gzip`, when a compressed copy was sent  |
| `Vary`                    | Always `Accept-Encoding`                         |
| `Content-Security-Policy` | The site's own policy, or npanel's               |
| `X-Nsite-Addr`            | Which nsite this hostname serves, as an `naddr`  |
| `Onion-Location`          | The site's onion address, when it has one        |

`X-Nsite-Addr` is the one header naming the site itself. An `naddr` carries the
pubkey, kind, and `d` tag together, so it is a single value a client can paste
anywhere rather than three headers to reassemble.

`Content-Security-Policy` is whatever the site declared in the `<meta>` tags of
its `/index.html`, or a restrictive default when it declared nothing. It is the
same on every response for a host, documents and assets alike, and absent
entirely for a host whose [`csp` flag](#patch-apihostshostname) is off.

npanel stores compressed copies of a site's text files at deploy time and sends
whichever the client can read, preferring the smallest when the client states
no preference of its own. Each encoding has its own `ETag` — `"<sha256>-br"`,
`"<sha256>-gzip"`, or plain `"<sha256>"` for the blob — so a cache can't answer
one encoding's conditional request with another's. `Content-Length` and any
`Content-Range` describe the copy that was sent, not the blob.

`Onion-Location` is how Tor Browser offers a visitor the onion without anyone
having to hand the address out. It is sent only on a `200` carrying HTML, and
only over HTTPS — on a plain-HTTP hop anything in the path could rewrite it and
point the visitor at an onion of its own choosing. Requests that already arrived
over Tor never carry it.

Every site response is readable from any origin, headers included:
`Access-Control-Allow-Origin` and `Access-Control-Expose-Headers` are both `*`,
so a cross-origin `fetch` can revalidate against the `ETag`, resume from a
`Content-Range`, and read `X-Nsite-Addr` or `Onion-Location` like a same-origin
one. Preflights are answered for `GET`, `HEAD`, and `OPTIONS`. There is nothing
to configure — a site is public files with no credential attached.

### `/.well-known/nostr.json`

A host with [NIP-05 names](#get-apihostshostnamenip05) serves them here, in
place of any `nostr.json` its site deployed. A host with none serves the file
if the site published one, and otherwise `404`s like any other missing path.

```
GET https://example.com/.well-known/nostr.json?name=bob
{ "names": { "bob": "<hex>" }, "relays": { "<hex>": ["wss://relay.example.com"] } }
```

A `name` that this host doesn't answer for is `{ "names": {} }` with a `200` —
the question was answered, and the answer is that the domain doesn't know them.
A request with **no** `name` returns every name the host has, which is what lets
a crawler discover who a domain hosts; that is the same list a static
`nostr.json` would have handed over. `relays` is omitted when no name carries
one.

The response is `application/json`, cached for a minute, and readable from any
origin like every other site response — which NIP-05 requires, since a
browser-only client can't tell a CORS refusal from a missing file. It is served
even before the site behind the hostname has finished its first deploy: the
names belong to npanel, not to the manifest.

No redirect is ever issued here, per the NIP. Note that `www.example.com` still
redirects to `example.com` when that host asked for it, and a fetcher must
ignore the redirect — so publish `_@example.com`, not `_@www.example.com`.

Range requests are supported. A `503` with `Retry-After` means no version is
ready yet; a `404` means the hostname isn't configured, or the path isn't in the
version — served as the site's `/404.html` when it has one. On a host with `spa`
set, a document path that isn't in the version is a `200` carrying the site's
shell instead.
