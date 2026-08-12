# API

The dashboard is a client of this API and uses nothing private, so anything the
UI can do, a script can do.

Everything lives under `/api` on `DASHBOARD_HOST`. All responses are JSON,
including `404`s for routes that don't exist. Errors are `{ "error": "..." }`
with a 4xx or 5xx status. Request bodies are limited to 1 MB; anything larger
is `413`, refused before the signature is checked.

## Authentication

Every route except `/api/tls-check` requires [NIP-98](https://github.com/nostr-protocol/nips/blob/master/98.md)
auth. Sign a kind `27235` event and send it base64-encoded:

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

### `GET /api/tls-check?domain=<hostname>`

The one unauthenticated route, for Caddy's `on_demand_tls ask`. Returns `200`
with `ok` when the domain is `DASHBOARD_HOST` or an enabled host, and `404`
otherwise. It reveals only whether a hostname is configured.

It is also the one route that answers on any hostname rather than only on
`DASHBOARD_HOST`, because Caddy asks over the address it proxies to. Keep
npanel's port reachable only by the proxy — that is what `BIND_ADDRESS` is for.

Approvals are metered by `tls_check_limit_per_host` and `tls_check_limit_total`,
and once a limit is spent the route answers `429` with a `Retry-After` header.
Refusals aren't metered, and `DASHBOARD_HOST` is exempt.

## Session

### `GET /api/me`

```json
{ "pubkey": "<hex>", "npub": "npub1...", "role": "admin" }
```

## Hosts

A host object:

```json
{
  "id": 1,
  "hostname": "example.org",
  "pubkey": "<hex>",
  "kind": 35128,
  "identifier": "docs",
  "address": "35128:<hex>:docs",
  "naddr": "naddr1...",
  "enabled": true,
  "pinned_event_id": null,
  "created_by": "<hex>",
  "created_at": 1754000000,
  "updated_at": 1754000000,
  "active": { "...": "the version being served, or null" },
  "latest": { "...": "the newest known version, or null" }
}
```

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
versions are ever served, so `active` and `latest` differ while a deploy is in
flight or after one fails.

### `GET /api/hosts`

`{ "hosts": [...] }`, ordered by hostname. Staff see only their granted hosts.

### `POST /api/hosts`

```json
{ "hostname": "example.org", "address": "npub1..." }
```

`address` accepts an npub, nprofile, naddr, an `a` tag
(`35128:<pubkey>:<identifier>`), a NIP-5A label, or a URL containing one of
those. It's stored decomposed into pubkey, kind, and identifier.

`201` with `{ "host": {...} }`. `409` if the hostname is already configured or
is `DASHBOARD_HOST`. `400` if it doesn't sit under one of
`allowed_host_suffixes`, when that setting is non-empty. A staff user who
creates a host is granted it automatically, so they aren't locked out of their
own work.

### `GET /api/hosts/:id`

`{ "host": {...} }`, or `404` / `403`.

### `PATCH /api/hosts/:id`

Any subset of:

```json
{
  "hostname": "new.example.org",
  "address": "naddr1...",
  "enabled": false,
  "pinned_event_id": "<event id>"
}
```

Set `pinned_event_id` to an event id to freeze the host on that version, or
`null` to follow the newest ready one. Pinning a version that belongs to a
different site is `400`; pinning one that isn't fully downloaded is `409`.

Changing `address` clears any pin, since it belonged to the old site.

`hostname` is validated exactly as on create: `409` if it's already configured
or is `DASHBOARD_HOST`, `400` if it falls outside `allowed_host_suffixes`.

### `DELETE /api/hosts/:id`

`{ "ok": true }`. Removes the mapping; stored versions and blobs are untouched
until `prune-blobs` runs.

### `GET /api/hosts/:id/versions?limit=50`

```json
{
  "active_event_id": "<hex or null>",
  "pinned_event_id": "<hex or null>",
  "versions": [ ... ]
}
```

Newest first. `limit` is clamped to 1–200.

### `GET /api/hosts/:id/versions/:eventId/log`

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

### `GET /api/hosts/:id/files?event_id=<hex>`

Files in a version, defaulting to the active one (or the newest if none is
ready).

```json
{
  "event_id": "<hex>",
  "files": [
    { "path": "/index.html", "sha256": "<hex>", "size": 1024, "stored": true }
  ]
}
```

`stored` is false for a blob that hasn't been downloaded yet — which is how you
spot exactly what a stalled deploy is missing.

### `POST /api/hosts/:id/sync`

Fetch the newest manifest from relays now. Returns `202` immediately;
relay round trips take seconds, so poll the host or its versions for the result.

### `GET /api/hosts/:id/grants` — admin

`{ "grants": [{ "user_pubkey": "<hex>", "created_at": 1754000000 }] }`

### `PUT /api/hosts/:id/grants` — admin

```json
{ "pubkeys": ["<hex>", "<hex>"] }
```

Replaces the grant list wholesale. Every pubkey must be hex and must already be
an npanel user. Returns `{ "grants": [...] }`.

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
      "host_ids": [1, 4]
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
admin out of the dashboard through the API. (`bun run make-admin` is the way
back in if it ever happens anyway.)

## Settings — admin

### `GET /api/settings`

```json
{
  "settings": {
    "relays": ["wss://relay.ditto.pub"],
    "blossom_fallback_servers": ["https://blossom.ditto.pub/"],
    "max_file_size": 104857600,
    "resync_interval": 600,
    "allowed_host_suffixes": [],
    "tls_check_limit_per_host": 60,
    "tls_check_limit_total": 600
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
  "relays": ["wss://relay.ditto.pub"]
}
```

Available to any authenticated user.

## Site responses

Not part of the JSON API, but worth knowing when debugging a served site:

| Header               | Meaning                                   |
| -------------------- | ----------------------------------------- |
| `ETag`               | The blob's sha256, quoted                 |
| `Last-Modified`      | The manifest event's `created_at`         |
| `X-Npanel-Pubkey`    | The nsite author                          |
| `X-Npanel-Event`     | The exact version served                  |
| `X-Npanel-Identifier`| The `d` tag, for named sites              |

Range requests are supported. A `503` with `Retry-After` means no version is
ready yet; a `404` means the hostname isn't configured, or the path isn't in the
version and the site has no `/404.html`.
