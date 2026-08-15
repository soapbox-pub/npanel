# Deploying the soapbox instance

This is the runbook for the gateway at **npanel.soapbox.pub**, running on
`venus.vps`. It records what that box actually looks like, because two things
there differ from the defaults and both of them break startup if forgotten.

For self-hosting npanel anywhere else, read
[docs/deployment.md](docs/deployment.md) instead — that one is written for
someone standing up their own gateway, and this one assumes you have `ssh
venus.vps` and `sudo`.

## The box

Ubuntu 24.04, Caddy 2.11, Bun at `/usr/local/bin/bun`.

| Thing         | Where                                                      |
| ------------- | ---------------------------------------------------------- |
| Checkout      | `/opt/npanel`, owned by `npanel:npanel`                    |
| Data          | `/var/lib/npanel` — `npanel.sqlite3`, `blobs/`, `backups/` |
| Unit          | `/etc/systemd/system/npanel.service`                       |
| Environment   | `/opt/npanel/.env`, mode 640, read by the unit             |
| Proxy         | `/etc/caddy/Caddyfile` + `/etc/caddy/sites-available/*`    |

The checkout's `origin` is the `nostr://` remote, and `git-remote-nostr` is
installed at `/usr/local/bin`, so the `npanel` user can fetch without any PATH
fixing.

## The two things that are not the defaults

**Ports.** venus already runs ditto on 3000, a docker-proxy for nostr-push on
3001, and agora on 3002. npanel therefore listens on **3003**, and its
certificate-authorization listener on **3004**. Both are loopback-only; Caddy is
the only thing that reaches either.

`TLS_CHECK_PORT="3004"` must stay in `.env`. Its default is 3001, which is the
docker-proxy — leave it unset and `Bun.serve` fails to bind, systemd restarts
the service on a loop, and the gateway is down. `PORT` and `TLS_CHECK_PORT`
being equal is refused at startup with a message that says so; a port taken by
something else is an `EADDRINUSE` at bind time.

**No `sqlite3` binary.** The backup command in
[docs/deployment.md](docs/deployment.md#backups) assumes one. Use Bun instead —
`VACUUM INTO` is an online backup and is safe against the running server:

```sh
ssh venus.vps
cat > /tmp/npanel-backup.ts <<'EOF'
import { Database } from "bun:sqlite";
const stamp = new Date().toISOString().slice(0, 16).replace(/[-:T]/g, "");
const target = `/var/lib/npanel/backups/npanel-${stamp}.sqlite3`;
const db = new Database("/var/lib/npanel/npanel.sqlite3", { readonly: true });
db.exec(`VACUUM INTO ${JSON.stringify(target)}`);
db.close();
console.log(target);
EOF
sudo -u npanel /usr/local/bin/bun run /tmp/npanel-backup.ts
```

## Upgrading

```sh
ssh venus.vps

# 1. Back up the database first — migrations run at startup.
#    (the snippet above)

# 2. Code.
cd /opt/npanel
sudo -u npanel git fetch origin
sudo -u npanel git merge --ff-only origin/main

# 3. Dependencies and the dashboard. The server needs no build step; the
#    dashboard does, and a missing dist/ means the dashboard host answers 503.
sudo -u npanel /usr/local/bin/bun install --frozen-lockfile
sudo -u npanel /usr/local/bin/bun run dashboard:install
sudo -u npanel /usr/local/bin/bun run dashboard:build

# 4. Restart. Expect a second or two where Caddy has nothing to proxy to.
sudo systemctl restart npanel
systemctl is-active npanel
sudo journalctl -u npanel -n 20 --no-pager -o cat
```

A restart is not graceful from a visitor's point of view: the port stops
accepting for a moment and Caddy answers `502`. Nothing is lost — the sync
worker's writes are transactional and blobs are content-addressed — but do it
deliberately rather than in a loop.

## Verifying

```sh
# Both listeners bound, on loopback only.
sudo ss -ltnp | grep -E ":(3003|3004) "

# Certificate authorization: ok for the dashboard and for any enabled host,
# 404 for anything else, and nothing else served on that port.
curl -s "http://127.0.0.1:3004/tls-check?domain=npanel.soapbox.pub"   # ok
curl -s "http://127.0.0.1:3004/tls-check?domain=not-configured.test"  # unknown host
curl -s "http://127.0.0.1:3004/"                                      # not found

# The public surface, through Caddy.
curl -sI https://npanel.soapbox.pub/ | head -1
curl -sI https://ditto.pub/ | head -1
```

To confirm Caddy is really reaching the authorization port rather than failing
to and refusing everything — the two look identical from outside — check the
running config and watch for the connection:

```sh
curl -s http://127.0.0.1:2019/config/apps/tls/automation | grep -A3 on_demand
# => "endpoint": "http://127.0.0.1:3004/tls-check"

sudo ss -tan state time-wait | grep -c ":3004"
openssl s_client -connect 127.0.0.1:443 -servername nobody.example </dev/null >/dev/null 2>&1
sudo ss -tan state time-wait | grep -c ":3004"   # one higher: Caddy asked
```

## Caddy

The global block names the authorization endpoint; on-demand issuance itself is
off unless a site block opts in, and only `zz-nsite-catchall` does:

```caddy
on_demand_tls {
	permission http http://127.0.0.1:3004/tls-check
}
```

`npanel.soapbox.pub` has its own file in `sites-available/` and gets an ordinary
managed certificate. Every other hostname pointed at venus is matched by
`zz-nsite-catchall` — the `zz-` prefix is load-bearing, since the files are
imported in glob order and the catch-all has to come last.

Keep a copy before editing (`/etc/caddy/backups/`), then:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Reloading restarts certificate maintenance, so the log immediately fills with
retries for names whose issuance is already failing. As of this writing
`agora.land`, `www.armada.buzz`, and `www.gleasonator.dev` fail on every reload
— NXDOMAIN for the two `www` names, and a Cloudflare DNS-01 error for
`agora.land`. Neither has anything to do with npanel; they are managed certs
from their own site blocks, not on-demand ones.

## When something is wrong

- **Service restart-looping.** Almost always a port. `journalctl -u npanel -n 20`
  shows the bind error; check `ss -ltnp` for whoever took 3003 or 3004.
- **Every new hostname is refused a certificate.** Caddy's `permission` endpoint
  is wrong or unreachable. It must be `:3004/tls-check`; the pre-August-2026
  spelling `:3003/api/tls-check` answers `401` now, and Caddy reads anything
  that isn't 2xx as "no".
- **Dashboard answers 503.** `dashboard/dist` is missing — run the build step.
- **A site answers 503.** No version is ready. Open the version's deploy log in
  the dashboard; usually a blob no server still has.
- **The Cloudflare token** lives in the Caddyfile. Do not paste that file into
  anything public.
