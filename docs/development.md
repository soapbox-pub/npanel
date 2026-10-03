# Development

A local npanel is the production one with the reverse proxy left out. It serves
plain HTTP on a port, routes on `Host` exactly as it does in production, and
talks to real relays and real Blossom servers — so a site you add locally is a
site somebody actually published.

The only piece that works differently is the dashboard. In production the server
serves the built SPA out of `dashboard/dist`; in development Vite serves it with
hot reload on its own port and proxies `/api` to the server.

## Setup

Two toolchains: [Rust](https://rustup.rs) for the server, and
[Bun](https://bun.sh) 1.3+ for the dashboard.

```sh
cargo build
(cd dashboard && bun install)
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
cargo run -- make-admin npub1yourkeyhere
```

## Running

In two terminals:

```sh
cargo run                        # the server
(cd dashboard && bun run dev)    # the dashboard, with hot reload
```

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

A debug build is several times slower than a release one, most visibly in
user-agent parsing and brotli compression. Use `cargo run --release` when
measuring anything.

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

**`http://localhost:3000` answers `503`** until the dashboard is built. That is
the server serving `dashboard/dist` off disk, which `bun run dev` never builds
(a release binary is built with `embedded-dashboard` and carries its own; see
[deployment](deployment.md#from-source)). The
dashboard is at `:8080`. Run `bun run build` in `dashboard/` if you want to
check the production SPA — that path serves the built `index.html`, rewrites its
link-preview meta tags, and caches Vite's hashed assets forever, none of which
the dev server does. The running server picks the build up without a restart.

**Absolute URLs point at `https` and drop the port.** npanel terminates no TLS
and assumes `https` unless a proxy says otherwise, and a hostname carries no
port through the code. So the `www` redirect, the canonical `Link` header, and
the sitemap all name `https://foo.localhost/…` even though you are reading them
on `:3000`. Browsing works; those three features cannot be exercised without a
proxy in front. The tests in `src/seo.rs` state the origin outright instead,
which is where to check a change to any of them.

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
cargo clippy --all-targets              # no warnings
cargo test                              # the server
cargo test --manifest-path vendor/device-detector/Cargo.toml
(cd dashboard && bun run test)          # tsc, eslint, vitest, and a production build
```

`.ngit/act/workflows/test.yml` runs the same on every push and Nostr PR, plus
clippy and the dashboard tests with `--features embedded-dashboard`, which
needs `dashboard/dist` built first.

Server tests live beside their subject in a `#[cfg(test)]` module.
`src/e2e.rs` assembles a whole gateway the way `main` does, with a fake Nostr
source and a Blossom server on loopback, and drives a deploy from a manifest to
a served site and the API — the place for a test that crosses subsystems.

Note the `run` in `bun run test`: a bare `bun test` starts Bun's own test runner
instead of the script, and the dashboard's tests need Vitest's DOM environment.

## The dashboard on another origin

The dashboard published as an nsite has no API on its own origin, and asks
which server to manage. To try that locally, serve a production build from
anywhere else and point it at the dev server:

```sh
(cd dashboard && bun run build && cd dist && python3 -m http.server 8099)
```

Open `http://127.0.0.1:8099`, and connect to `http://localhost:3000`. The
dashboard's Content Security Policy admits `http://localhost:*` and
`http://127.0.0.1:*` for exactly this; everything else has to be https.

## Releasing

A tag `vX.Y.Z` runs `.ngit/act/workflows/release.yml`, which builds the static
binary, publishes it as a NIP-82 release with `ngit release publish` (what to
publish is in `.ngit/release.yaml`), and publishes the dashboard nsite with
`ngit nsite publish` (`.nsite/config.json`). Before tagging:

1. Bump `version` in `Cargo.toml`. The workflow refuses a tag that disagrees
   with it, and the dashboard stamps it in at build time to compare with the
   server's.
2. Add a `## [X.Y.Z]` section to `CHANGELOG.md`. It becomes the release notes,
   and the workflow refuses a tag without one.
3. Commit, tag with `git tag -a vX.Y.Z`, and push both.

A tag with a suffix, like `v0.3.0-rc.1`, publishes to the `beta` channel rather
than `main`. Both are signed by the `NSYTE_BUNKER` session provisioned for the
repository, and that key owns the application: every release after the first
has to be signed by it.

To build the release binary locally, which needs `musl-tools` (for `musl-gcc`):

```sh
(cd dashboard && bun run build)
cargo build --profile dist --target x86_64-unknown-linux-musl --features embedded-dashboard
```

`dist` is `release` with fat LTO and one codegen unit (see `Cargo.toml`): a
smaller, faster binary, from a build that takes several minutes every time. Plain `--release` rebuilds in seconds, and is what to use for anything
short of publishing.

## Housekeeping

A server prunes once a day, ten minutes after it starts, which a development
server you keep restarting may never reach. The production tools work locally:

```sh
cargo run -- prune-blobs --keep 3 --dry-run
cargo run -- prune-blobs --keep 3
```

To start over entirely, stop the server and delete `data/`. The next start
migrates a fresh database, and you will need `make-admin` again.
