# Development

A local npanel is the production one with the reverse proxy left out. It serves
plain HTTP on a port, routes on `Host` exactly as it does in production, and
talks to real relays and real Blossom servers — so a site you add locally is a
site somebody actually published.

The only piece that works differently is the dashboard. In production the server
serves the built SPA out of `dashboard/dist`; in development Vite serves it with
hot reload on its own port and proxies `/api` to the server.

## Setup

[Bun](https://bun.sh) 1.3+ is the only prerequisite. Nothing here needs Node,
npm, or Docker.

```sh
bun install
bun run dashboard:install
```

Then the configuration. It is the same `.env` production uses, with local
values:

```sh
cp .env.example .env
```

```
PORT="3000"
BIND_ADDRESS="127.0.0.1"
TLS_CHECK_PORT="3001"
ANALYTICS_PORT="3002"
DASHBOARD_HOST="localhost"
LOG_LEVEL="debug"
DATA_DIR="./data"
```

`DASHBOARD_HOST="localhost"` is the one that matters, and `debug` logging is
worth it here — the sync worker explains each blob it fetches and each path it
fails to find, which is most of what you want to know when a site doesn't look
the way you expected. `./data` is in `.gitignore`.

npanel is not permissionless, so an unknown pubkey gets a `403` locally exactly
as it would in production. Bootstrap yourself:

```sh
bun run make-admin npub1yourkeyhere
```

## Running

```sh
bun run dev
```

That starts both halves, with prefixed output and a single Ctrl-C:

| | |
| --- | --- |
| `http://localhost:8080` | the dashboard, served by Vite with hot reload |
| `http://localhost:3000` | the server: the API, and every site you add |

Work at `:8080`. Sign in with a NIP-07 browser extension — `http://localhost` is
a secure context, so extensions offer to sign there like anywhere else.

The dashboard's requests go through Vite's proxy, which preserves the `Host`
header, and NIP-98 compares only the hostname of the URL you signed for — never
the port or the scheme — so `http://localhost:8080/api/hosts` verifies against
`DASHBOARD_HOST=localhost` with nothing to configure.

Either half can be run alone. `bun run dev:server` is the server on its own,
which is what you want when you're changing how the built SPA is served rather
than the SPA itself; `bun run dashboard:dev` is the reverse.

## Serving a site locally

Add a host in the dashboard the way staff would, and give it a hostname under
`.localhost`:

```
foo.localhost  →  npub1... (or an naddr, an a tag, or an nsite URL)
```

Then visit `http://foo.localhost:3000/`. Nothing usually needs adding to
`/etc/hosts`: RFC 6761 reserves every name under `.localhost` for loopback, and
browsers and modern resolvers honour that without being told. npanel accepts one
as a hostname because `allowed_host_suffixes` is empty by default.
The site is fetched from real relays and Blossom servers, so this needs
internet — there is no offline path for a manual test. Watch the log, or open
the version's deploy log in the dashboard, to see it happen.

Requests are routed entirely on `Host`, so `curl` reaches a site whose name
doesn't resolve at all:

```sh
curl -H 'Host: example.org' http://localhost:3000/
```

## What differs from production

**`http://localhost:3000` answers `503`.** That is the server serving
`dashboard/dist`, which development never builds. The dashboard is at `:8080`.
Run `bun run dashboard:build` if you want to check the production SPA — that
path serves the built `index.html`, rewrites its link-preview meta tags, and
caches Vite's hashed assets forever, none of which the dev server does.

**Absolute URLs point at `https` and drop the port.** npanel terminates no TLS
and assumes `https` unless a proxy says otherwise, and a hostname carries no
port through the code. So the `www` redirect, the canonical `Link` header, and
the sitemap all name `https://foo.localhost/…` even though you are reading them
on `:3000`. Browsing works; those three features cannot be exercised without a
proxy in front. `src/seo.test.ts` states the origin outright instead, which is
where to check a change to any of them.

**Certificate authorization has nobody asking.** `TLS_CHECK_PORT` is listening,
and answers by hand:

```sh
curl 'http://127.0.0.1:3001/tls-check?domain=foo.localhost'
```

**Tor is off**, as it is on a fresh production instance. Turning it on under
Settings needs a local Tor daemon with a control port; see
[deployment](deployment.md#tor).

To reach the dev server from another device — a phone, a VM — set
`BIND_ADDRESS="::"`, which binds every interface for both IPv4 and IPv6. Note
that this exposes `TLS_CHECK_PORT` to the same network, and that endpoint is
unauthenticated by design.

## Checks

Both toolchains, and both must pass:

```sh
bun lint                 # biome
bun check                # tsc --noEmit, over src and scripts
bun test                 # node:test, run through bun
bun run dashboard:test   # tsc, eslint, vitest, and a production build
```

Server tests live next to their subject as `*.test.ts` and use the harness in
`src/test/harness.ts` — an in-memory database, a fake Nostr service, and a
signed-request helper — rather than a real relay.

Note the `run` in `bun run dashboard:test`. From inside `dashboard/` it has to
be `bun run test` too: a bare `bun test` starts Bun's own test runner instead of
the script, and those tests need Vitest's DOM environment.

## Housekeeping

Every version of every site is kept, along with its files, so a data directory
you have been testing against grows. The production tools work locally:

```sh
bun run prune-blobs --keep 3 --dry-run
bun run prune-blobs --keep 3
```

To start over entirely, stop the server and delete `data/`. The next start
migrates a fresh database, and you will need `make-admin` again.
