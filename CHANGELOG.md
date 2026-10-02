# Changelog

Release notes, one section per version. `ngit release publish` takes the
section matching the tag it publishes, so every release needs one here before
it is tagged.

## [Unreleased]

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
- Building from source needs Rust 1.92 or newer.

## [0.2.0]

The first release.

- A static `x86_64-unknown-linux-musl` binary that runs on any Linux
  distribution, with the dashboard compiled in, so a release is one file. Other
  architectures still build from source.
- The dashboard is published as an nsite too, and can manage any npanel server
  from there: it asks which one, and talks to it across origins.
- `/api/instance` reports the server's version, so a dashboard served from
  elsewhere can say when the server is behind it.
