# Changelog

Release notes, one section per version. `ngit release publish` takes the
section matching the tag it publishes, so every release needs one here before
it is tagged.

## [0.6.1]

- A preview script can name the page's canonical URL with `canonical`, for
  sites where several paths show one thing — `/npub1…`, `/nprofile1…` and
  `/name@domain` for one profile. It's written as `<link rel="canonical">`
  and `og:url`, and must be on the same site.
- Links in a preview's article to the site's own pages are no longer
  `nofollow`, so a crawler can follow them to the rest of the site. Links
  elsewhere still are.

## [0.6.0]

- **Breaking:** npanel reads its configuration from a TOML file instead of
  `.env` and the environment. It uses the file `--config` names, or else
  `~/.config/npanel/npanel.toml` or `/etc/npanel/npanel.toml`, and refuses to
  start with none. The settings are the same seven, named in lowercase:
  `DASHBOARD_HOST="panel.example.com"` becomes
  `dashboard_host = "panel.example.com"`, and ports are numbers. A key npanel
  doesn't know is refused. `npanel.example.toml` replaces `.env.example`, and
  "From `.env` to `npanel.toml`" in docs/deployment.md says how to move an
  install across. The sample unit names the file with `--config` and no
  longer sets a working directory, so `sudo -u npanel npanel make-admin`
  works from anywhere.
- **Breaking:** with no `data_dir`, data goes in `/var/lib/npanel` as root
  and `~/.local/share/npanel` otherwise, rather than `./data`. A relative
  `data_dir` is relative to the config file.
- On Docker, `compose.yaml` writes npanel's config file, from `DASHBOARD_HOST`
  and `LOG_LEVEL` in compose's `.env` as before; the image sets no environment
  variables, and the health check moved from the image to `compose.yaml`.
- Each release publishes `.deb` and `.rpm` packages for x86_64 and aarch64,
  which install the binary, the systemd unit and `/etc/npanel/npanel.toml`,
  and create the `npanel` user and `/var/lib/npanel`. `SHA256SUMS` and the
  sample unit are no longer published as assets: the release's asset events
  carry each file's SHA-256, and the unit is `contrib/npanel.service`.
- `npanel --help` and `npanel --version`, and help for each command.
- Tor's warnings about onion services are debug lines: each was one failure
  Tor retries on its own, and a gateway with a dozen sites logged hundreds an
  hour while every one stayed reachable. In their place, npanel warns with
  `tor_onion_unreachable` when a site's onion has been unreachable for ten
  minutes, saying why, and logs `tor_onion_reachable` when it is back.
- The Docker image runs the same static binary a release publishes, built by
  the same script, `contrib/release/build.sh`, with the dashboard compiled in.
  That script, with `contrib/release/tools.sh` for its pinned tools, is how to
  build npanel for a server by hand too.

## [0.5.0]

- Analytics can be counted on the server instead of by a script. Turn off
  **Tracking script** under a site's Analytics switch, and its pages go out as
  published, with nothing added: npanel counts each page a browser opens as it
  serves it, under the same cookieless, daily-salted visitor hash. Visits,
  sources, locations and devices are counted as before; moves within a
  single-page app, time on page and scroll depth can't be, and only requests a
  browser marks as opening a page (`Sec-Fetch-Mode: navigate`,
  `Sec-Fetch-Dest: document`, not a prefetch) count. Each site is counted one
  way or the other, never both.

## [0.4.2]

- Files of 64 KiB and up are sent with `sendfile(2)` on Linux, straight from
  the page cache to the socket, when npanel serves plain HTTP behind a proxy:
  30–55% more requests a second for files of 128 KiB and up. The socket is
  corked while a file goes out, as nginx's `tcp_nopush` does, so the head and
  the file share whole segments. Smaller files, and connections over TLS or
  Tor, are sent as before.
- npanel raises its soft limit on open files to the hard limit at startup,
  since large files being served are now kept open.

## [0.4.1]

- The error reporter scrubs Nostr secret keys and secret URL parameters in the
  browser, before a report is sent, as well as on arrival. A page's fragment
  and its state reach npanel only in a report, so a key in either no longer
  crosses the network at all.

## [0.4.0]

- Error tracking, built in. Turn on **Error reporting** in a site's Privacy &
  Security settings, and its pages load a small reporter, from the site's own
  origin, that sends what they throw to npanel. Reports are grouped into
  issues, listed under the site's **Errors** with how often each happens and
  to how many visitors, and opened one event at a time: the stack trace, what
  the visitor clicked, fetched and logged before it, the page, the browser and
  the version. Issues can be resolved and ignored.
- Stack traces are mapped through the source maps the site deployed beside its
  scripts — there is nothing to upload — so they show the original files,
  functions and lines, and one bug stays one issue across deploys.
- The endpoint speaks Sentry's envelope format, so a site already using a
  Sentry SDK can point its DSN at `https://<key>@<site>/_npanel/0` instead.
- Nostr secret keys, and URL parameters named like secrets (`secret=`,
  `access_token=`, `api_key=`…), are scrubbed from every report, and visitors
  are counted under the same daily salts as analytics. A site may send 60
  reports a minute; reports are kept for `errors_retention_days`, 90 by
  default.
- The `dist` build profile is gone: `--release` now builds what releases
  publish, with full LTO. Build from source with `cargo build --release`, and
  install `target/release/npanel`.

## [0.3.1]

- A preview script can change a `URL`: its `href`, `pathname`, `search` and
  the rest set as they do in a browser, and `url.searchParams.set`, `append`,
  `delete` and `sort` change the URL they came from. Both threw before.
  `URLSearchParams` also writes a space as `+` and reads a stray `%` as it is,
  as a browser does.

## [0.3.0]

- Link previews drawn by the site. A site whose pages a crawler can't read by
  itself — a Nostr client showing `/npub1…` or `/nevent1…` from relays in the
  browser, say — publishes one self-contained script at
  `/.well-known/npanel/preview.js` and turns on **Link previews from the site's
  script**. The script lists the pages it previews as URLPattern `routes`, or
  previews every page. Crawlers then get whatever it makes of a page: its
  title, description and image as Open Graph, Twitter and schema.org tags, and
  an article a search engine can index. Everyone else gets the page as
  published. This replaces routing crawlers to a separate server from the
  proxy.
- The script is handed the crawler's request and a read-only Nostr relay,
  shaped like Nostrify's, and can draw its image on an `OffscreenCanvas`, which
  npanel serves at `/_npanel/preview/…`. The page never waits for its image.
  Each run is metered and given a deadline, in a process of its own that on
  Linux can't open a file, make a socket or start a program (Landlock and
  seccomp), and that is killed if it overruns: a preview that can't be had
  within a couple of seconds sends the page as published, never an error. A
  script runs only on a site with link previews on, deploy checks included. A deploy's log says whether the script
  loads. See "Link previews" in `docs/architecture.md`.
- Previews draw text in the fonts installed on the system, emoji included. The
  Docker image comes with DejaVu and Noto Color Emoji; a systemd install
  should `apt install fonts-dejavu-core fonts-noto-color-emoji`.
  `docs/deployment.md` says how to add others.
- Previews and their images are cached in `DATA_DIR/previews.sqlite3` and
  `DATA_DIR/preview-images/`, scripts' queries are batched into one
  subscription per relay, and runs are capped per minute.
- **Settings → Link Previews** shows what is cached and how lookups have gone,
  runs one page's script again from its URL, clears the cache, and sets which
  User-Agents count as crawlers.
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
- Releases include a static `aarch64` Linux binary as well as the `x86_64` one,
  and a `SHA256SUMS` for both.
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
