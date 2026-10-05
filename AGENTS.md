# npanel

A self-hostable nsite gateway with a team dashboard. Staff map hostnames to
nsites in a web UI; npanel downloads each site's blobs from Blossom servers and
serves them from local storage.

Read [docs/architecture.md](docs/architecture.md) before making non-trivial
changes — it explains the sync worker, the deploy invariant, and why storage
looks the way it does.

## Layout

```
src/
  main.rs               entry point; wires everything together, and the CLI
  app.rs                Host-based routing: dashboard vs nsite
  server.rs             serving HTTP/1.1 on a listener
  config.rs             the seven environment variables, and .env
  log.rs                one flat JSON object per line
  hostname.rs, psl.rs   hostname validation and root-domain rules
  net.rs                public-address checks, and the resolver downloads use
  rate_limit.rs         token buckets, used to meter certificate authorization
  util.rs               small helpers used throughout
  settings.rs           runtime settings, DB-backed and live-reloading
  nsite_address.rs      address parsing
  identifiers.rs        relay URLs, pubkeys, NIP-05 names
  manifest.rs           manifest event parsing
  mime.rs               content types by file extension
  html.rs               decoding HTML attribute values
  nostr/                events, NIP-19, the relay pool, and relay lookups
  sync.rs               the sync worker: subscribe, resync, deploy
  builds.rs             host builds: the frozen bundle each host serves
  prune.rs              forgetting old versions and deleting unused blobs, daily
  deploy.rs             the runtime sync and builds run on, and its handle
  supervise.rs          a runtime on a thread of its own, restarted if it dies
  tls_check.rs          certificate authorization, on its own port
  tls/                  HTTPS without a proxy: ACME certificates, renewed
  analytics/            the Plausible-compatible analytics runtime
  link_previews/        sites' preview scripts, run for crawlers on a runtime of its own
  blobs.rs              content-addressed filesystem store
  hash.rs               BlobHash, a blob's name, parsed where it enters
  sites.rs              queries over stored versions, and the deploy log
  site_csp.rs           a site's declared Content Security Policy
  site_preview.rs       link-preview metadata, read from a site's own index.html
  seo.rs                sitemaps, robots.txt, canonical links
  serve.rs              serving an nsite request, and its caches
  dashboard.rs          serving the built SPA, from disk or compiled in
  auth.rs               NIP-98
  api.rs                the JSON API
  dns.rs                what staff should point a hostname at
  tor.rs                onion services, on Tor built in (Arti)
  db/                   pool, migrations, rows
  scripts.rs            make-admin, prune-blobs, compress-blobs
  e2e.rs                end-to-end tests with a fake Nostr and Blossom
build.rs                compiles dashboard/dist in, with `embedded-dashboard`
vendor/device-detector/ Matomo's device detector, vendored and fixed (see its README)
assets/                 data compiled into the binary: the PSL, Matomo's
                        database, Plausible's tracker and source lists
dashboard/              the SPA (React, Vite, its own package.json)
docs/                   deployment, configuration, architecture, api, development
contrib/npanel.service  the systemd unit each release ships
.ngit/                  CI workflows, and what a release publishes
.nsite/config.json      where the dashboard's nsite is published
CHANGELOG.md            release notes, one section per version
```

Releases are cut by tagging; see "Releasing" in docs/development.md. A tag
needs a matching `Cargo.toml` version and `CHANGELOG.md` section.

## Conventions

- Fix problems at the root. No bandaid fixes, no dead code left behind, no
  `unwrap()` on anything a request or a relay can influence.
- Comments explain *why*, not *what*. Match the density of the surrounding code.
- Nothing blocking on a runtime thread. Database work goes through `Db::call`,
  which runs on the blocking pool; CPU-heavy work (compression, user-agent
  parsing) goes through `spawn_blocking`. Don't hold a `parking_lot` guard
  across an `.await`.
- Only seven things are environment variables. Everything else belongs in the
  `settings` table so admins can change it without a restart. Adding a setting:
  a field in the `settings!` list, a default, a branch in `validate`, and a
  subscriber if anything reacts to it. Then surface it on the dashboard's
  Settings page.
- Migrations are append-only: existing deployments' databases have already
  run the old ones.
- The dashboard has TypeScript twins of the server's parsing rules in
  `dashboard/src/shared/`, each naming the Rust module it mirrors. Change both
  together.
- Logs are one flat JSON object per line. Use the `Logger` and the `fields!`
  macro; no `println!` in server code (the maintenance commands print to the
  terminal, which is their job).
- User-facing error strings are full sentences that say what to do next. They
  show up in the dashboard.

## Verifying changes

```sh
cargo clippy --all-targets    # no warnings
cargo test
cargo test --manifest-path vendor/device-detector/Cargo.toml
(cd dashboard && bun run test)
cargo clippy --all-targets --features embedded-dashboard   # needs dashboard/dist
```

From inside `dashboard/` it has to be `bun run test` rather than `bun test`: a
bare `bun test` starts Bun's test runner instead of the script, and the
dashboard's tests need Vitest's DOM environment.

## Committing

Always commit when the work is finished and the checks above pass.
