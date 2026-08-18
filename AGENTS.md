# npanel

A self-hostable nsite gateway with a team dashboard. Staff map hostnames to
nsites in a web UI; npanel downloads each site's blobs from Blossom servers and
serves them from local storage.

Read [docs/architecture.md](docs/architecture.md) before making non-trivial
changes — it explains the sync worker, the deploy invariant, and why storage
looks the way it does.

## Layout

```
src/                    server (Bun/Node, Hono, Kysely)
  server.ts             entry point; wires everything together
  app.ts                Host-based routing: dashboard vs nsite
  config.ts             the seven environment variables
  hostname.ts           hostname validation and root-domain rules (tldts)
  rate-limit.ts         token buckets, used to meter certificate authorization
  bunker/               the NIP-46 gateway, in its own worker thread
  settings.ts           runtime settings, DB-backed and live-reloading
  nsite-address.ts      address parsing; also imported by the dashboard
  manifest.ts           manifest event parsing
  nostr.ts              relay pool, relay/server list caches
  sync.ts               the sync worker: subscribe, resync, deploy
  tls-check.ts          certificate authorization, on its own port
  blobs.ts              content-addressed filesystem store
  sites.ts              queries over stored versions
  site-preview.ts       link-preview metadata, read from a site's own index.html
  serve-site.ts         serving an nsite request
  dashboard.ts          serving the built SPA
  auth.ts               NIP-98
  api/                  the JSON API
  db/                   dialect, schema, migrations
  test/harness.ts       shared test harness
  test/nip46.ts         a fake NIP-46 signer, for bunker tests
scripts/                make-admin.ts, prune-blobs.ts
dashboard/              the SPA (React, Vite, its own package.json)
docs/                   deployment, configuration, architecture, api
```

## Runtimes

Bun is the primary runtime, but the server code must stay portable across Bun,
Node, and Deno.

- **Use `node:` imports for builtins.** `import { readFile } from "node:fs/promises"`,
  `import process from "node:process"`.
- **Never use Bun or Deno globals** — no `Bun.file`, no `Deno.readFile`, no
  `Bun.env` — with the single exception of serving HTTP, below.
- **Never use `node:http`.** Not in `src`, not in tests. Serving HTTP is the one
  place where the runtime-specific API is strongly preferred: Bun implements
  `node:http` as a shim over its own server, and its stream-based request model
  is a second, worse HTTP world alongside the `Request`/`Response` objects the
  rest of the code already passes around. It buys portability nobody collects,
  since a Node or Deno deployment replaces the listener anyway.

  Use the default export from `src/server.ts` where one listener is enough, and
  `Bun.serve` where a second port has to be bound (`src/tls-check.ts`). Type it
  with a narrow `declare const Bun` covering only the API that file uses, so
  `tsc` still rejects every other Bun global.
- SQLite goes through the adaptive dialect in `src/db/sqlite.ts`, which picks
  `bun:sqlite` or `node:sqlite` at runtime. **Do not add `better-sqlite3`** — it
  crashes Bun's NAPI implementation.

Those rules are about keeping the *source* portable. The toolchain itself is
Bun only — installing, running, testing, and building the dashboard all go
through Bun, and nothing in the repo needs Node or npm installed.

## Configuration

Only seven things are environment variables: `PORT`, `BIND_ADDRESS`,
`TLS_CHECK_PORT`, `DASHBOARD_HOST`, `SECRET_KEY`, `LOG_LEVEL`, `DATA_DIR`.
Everything else belongs in the `settings` table so
admins can change it from the dashboard without a restart. `SECRET_KEY` is there
only because a key kept beside the rows it encrypts is not protecting them.

When adding a setting: add the key to `SettingsValues`, a zod schema to
`SCHEMAS`, a default to `DEFAULT_SETTINGS`, and — if anything needs to react to
it — a branch in the `settings.onChange` handler in `SyncWorker.start` or
`BunkerService.start`. Then surface it on the dashboard's Settings page.

## Tests

`node:test` with `node:assert/strict`, run through Bun:

```sh
bun test
```

Tests live next to their subject as `*.test.ts`. `bunfig.toml` pins the test
root to `src/`, without which Bun would walk into `dashboard/src` and fail on
tests that need a DOM. `src/test/harness.ts` provides
an in-memory database, a fake Nostr service, and a signed-request helper — use
it rather than standing up a real relay.

## Verifying changes

Server:

```sh
bun lint     # biome
bun check    # tsc --noEmit, covers src and scripts
bun test
```

Dashboard (its own toolchain — vite, eslint, vitest, all driven by Bun):

```sh
bun run dashboard:test
```

That runs `tsc --noEmit`, eslint, vitest, and a production build. From inside
`dashboard/` it has to be `bun run test` rather than `bun test`: a bare
`bun test` starts Bun's test runner instead of the script, and these tests need
vitest's DOM environment. The eslint
config is inherited from mkstack and is strict: no non-component exports from
component files, no `setState` in effects, no `FIXME` comments.

Run both before calling anything done.

## Committing

Always commit when the work is finished and the checks above pass. Never wait to
be asked — no exceptions.

## Conventions

- Fix problems at the root. No bandaid fixes, no dead code left behind, no `any`
  where a real type belongs.
- Comments explain *why*, not *what*. Match the density of the surrounding code.
- The dashboard imports shared parsing from the server via the `@shared/` alias
  (`vite.config.ts` + `tsconfig.json` paths). Keep address logic in
  `src/nsite-address.ts` and hostname rules in `src/hostname.ts` so the two
  can't drift.
- Logs are one flat JSON object per line. Use the `Logger` from `src/log.ts` and
  `errFields()` for errors; no `console.log` in server code.
- User-facing error strings are full sentences that say what to do next. They
  show up in the dashboard.
