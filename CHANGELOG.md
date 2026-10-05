# Changelog

Release notes, one section per version. `ngit release publish` takes the
section matching the tag it publishes, so every release needs one here before
it is tagged.

## [Unreleased]

- Link previews for Nostr pages. A site that shows Nostr content at the paths
  Nostr web clients use — `/npub1…`, `/nevent1…`, `/naddr1…`, `/name@domain` —
  can turn on **Link previews for Nostr pages**, and crawlers then get the
  event's own title, description and image, as Open Graph, Twitter and
  schema.org tags, and the post or article itself as text a search engine can
  index. Everyone else gets the page as published. This replaces routing
  crawlers to a separate server from the proxy.
- Every kind of event Ditto shows has a preview of its own, about a hundred
  of them: pictures, videos, music and podcasts (with `og:audio`), voice
  messages, articles, listings, calendar events, polls, highlights, zaps
  credited to their sender, reposts shown as the post they carry, follow packs,
  badges, git repositories and patches, apps, themes, and Ditto's own kinds.
  Content that isn't for reading — JSON, ciphertext, a memory card's hex —
  never reaches a preview, and an event behind a content warning shows none of
  what it warns of. An event of any other kind gets no preview: its page is
  served exactly as published.
- Lookups run on a thread of their own and never slow a site down: a preview
  that can't be had within a couple of seconds sends the page as published,
  never an error. Lookups are cached in `DATA_DIR/previews.sqlite3`, batched
  into one query per relay, and capped per minute.
- **Settings → Link Previews** shows what is cached and how lookups have gone,
  refreshes one preview from a pasted npub, nevent, NIP-05 name or URL, clears
  the cache, and sets which User-Agents count as crawlers.
- A single-page app now gets its shell for a path like `/@bob@example.com`,
  which was a `404` because `.com` read as a file extension.

- npanel can serve HTTPS itself, no proxy needed. Set `HTTPS_PORT=443` and
  `PORT=80`, and it gets a certificate from Let's Encrypt for the dashboard and
  every site it serves, `www` redirects included, the moment each is added, and
  renews them on its own. Plain HTTP answers challenges and redirects to HTTPS.
  The Caddy setup is unchanged and still the default.
- With it, HTTP/2, certificates and the ACME account kept in the database, a
  backoff per name that keeps a misconfigured domain inside Let's Encrypt's
  limits across restarts, and **Settings → Certificates** for the CA (with a
  switch for Let's Encrypt's staging CA), a contact email, and every name that
  is failing and why. Each site's page shows its certificates.
- `/api/status` has a `tls` field and each host a `certificates` field, both
  `null` behind a proxy.

- Files under `/.well-known/` are served with the content type their spec
  requires, so `apple-app-site-association` is JSON and Universal Links work.
  WebFinger, `host-meta`, NodeInfo, Matrix, OpenID/OAuth discovery, `lnurlp`
  and others are covered too. A `/.well-known/` path the site has no file for
  is now a plain `404`, never its `404.html` or SPA shell.

- npanel cleans up after itself. Once a day it keeps the newest 20 ready
  versions of each site, forgets the versions of sites no host points at, and
  deletes every stored file nothing uses. **Settings → Deploys → Versions kept
  per site** sets how many; 0 keeps them all. It is safe beside deploys in
  progress, so there is no `prune-blobs` timer to set up.
- `prune-blobs` now runs that same prune on demand. It applies the setting
  unless given `--keep N`, and `N` counts ready versions only: failed and
  superseded ones no longer take up places meant for versions you can roll
  back to.
- The dashboard shows each version's size and what a site stores, and
  **Settings** shows what is waiting to be cleaned up and what the last cleanup
  freed. Disk used now counts compressed copies too.

- Tor is built in. npanel runs Arti, the Tor Project's Rust implementation of
  Tor, on a thread of its own, so onion addresses need no Tor daemon, no control
  port and no `tor` container. Turning on **Settings → Tor** is the whole setup.
  Every site keeps the onion address it had.
- The `tor_control_url` and `tor_control_password` settings are gone. On Docker,
  `docker compose up -d --remove-orphans` takes down the old `tor` container,
  and the `npanel_tor-data` volume can be deleted. Tor's state now lives in
  `DATA_DIR/tor`.
- An onion only answers for its own address. A request arriving at one site's
  onion whose `Host` header names another site's onion now gets a 404 rather
  than that site.
- `/api/status` reports `tor.progress` while npanel joins the Tor network.
- The gateway's default Content Security Policy now allows WebAssembly
  (`'wasm-unsafe-eval'`), fonts from `blob:` and `data:`, and media from `blob:`
  and `data:`, as Isolated Web Apps' policy does. Sites that declare their own
  policy are unaffected.
- Building from source uses the Rust named in `rust-toolchain.toml`, which
  rustup installs on its own. Production builds use `--profile dist`; plain
  `--release` now rebuilds in seconds rather than minutes.
- The release binary is stripped and built with full LTO: 29 MB, down from 42.
- The dashboard loads in a fraction of the bytes. npanel serves its files
  brotli- or gzip-compressed itself, with `ETag`s so a reload revalidates
  rather than downloads, and each page is its own chunk: opening the site list
  fetches 900 KB of scripts and styles, 235 KB with brotli, where the whole
  1.75 MB bundle used to come first, uncompressed unless a proxy did it.

## [0.2.0]

The first release.

- A static `x86_64-unknown-linux-musl` binary that runs on any Linux
  distribution, with the dashboard compiled in, so a release is one file. Other
  architectures still build from source.
- The dashboard is published as an nsite too, and can manage any npanel server
  from there: it asks which one, and talks to it across origins.
- `/api/instance` reports the server's version, so a dashboard served from
  elsewhere can say when the server is behind it.
