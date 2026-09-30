//! Host builds: the frozen bundle each host serves.
//!
//! A build is a host's version chain flattened into one path map, plus every
//! file npanel generates or rewrites for that host — a sitemap, a `robots.txt`
//! with a line appended, `window.ENV` and the analytics tracker and every page
//! loading them. All
//! of it is worked out here, once, and stored as ordinary blobs with their
//! compressed copies, so serving a request is a lookup and a send whatever the
//! file is.
//!
//! Hosts are reconciled rather than tracked: every trigger — a deploy
//! finishing, a host being edited, an onion being minted, startup — asks what
//! each host's build *should* be made of, hashes that into a fingerprint, and
//! builds only where it differs from the build being served. A host serves
//! only a `ready` build, and a new build replaces the old one only once it is
//! complete.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256, Sha384};
use tokio::sync::watch;

use crate::analytics::tracker::{tracker_script, tracker_sha256, TRACKER_PATH};
use crate::blobs::BlobStore;
use crate::db::{BuildKind, Db, Host, HostBuild, SiteEvent, HOST_BUILD_COLUMNS};
use crate::fields;
use crate::inject::{inject_script, versioned_src, Placement, Script, MAX_INJECT_BYTES};
use crate::log::{with_err, Logger};
use crate::mime::is_compressible;
use crate::nsite_address::NsiteAddress;
use crate::seo::{augment_robots, robots_needs_sitemap, robots_txt, sitemap_paths, sitemap_xml, version_date, MAX_ROBOTS_BYTES};
use crate::serve::RequestOrigin;
use crate::settings::Settings;
use crate::site_env::{env_script, host_env, ENV_PATH};
use crate::sites::{
    active_event, host_by_id, newest_by_path, prune_deploy_log, record_blob, record_variant_sizes, version_chain, write_deploy_log,
};
use crate::util::extname;

/// Bumped whenever what a build produces changes for the same inputs. Part of
/// every fingerprint, so a new npanel whose builds would come out differently
/// rebuilds every host at startup.
///
/// The same number as the TypeScript npanel's: this port builds the same
/// bytes, so a gateway moving from one to the other rebuilds nothing.
pub const BUILDER_VERSION: i64 = 3;

/// Wait before a second attempt at a build. It doubles after that.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// One path of a build, as it goes into `host_build_files`.
#[derive(Clone, Debug)]
pub struct BuildFile {
    pub sha256: String,
    /// The version that published it, or `None` for a file npanel generated.
    pub event_id: Option<String>,
    /// The published blob it was rewritten from, if it was.
    pub source_sha256: Option<String>,
}

/// Everything one build of a host is made from.
#[derive(Clone)]
struct BuildTarget {
    host: Host,
    kind: BuildKind,
    /// The name and scheme its generated files address the site by.
    origin: RequestOrigin,
    /// The version it serves.
    event: SiteEvent,
    /// The versions it merges, newest first, starting with `event`.
    chain: Vec<String>,
    /// Whether its pages load the tracker.
    tracker: bool,
    /// The `window.ENV` script its pages load, if the host has variables.
    env: Option<String>,
    fingerprint: String,
}

impl BuildTarget {
    fn attempt_key(&self) -> String {
        format!("{}:{}:{}", self.host.id, self.kind.as_str(), self.fingerprint)
    }
}

/// The build a host is serving under one kind of name, if it has one: the
/// newest ready one.
pub fn current_build(conn: &Connection, host_id: i64, kind: BuildKind) -> rusqlite::Result<Option<HostBuild>> {
    conn.prepare_cached(&format!(
        "SELECT {HOST_BUILD_COLUMNS} FROM host_builds
         WHERE host_id = ? AND kind = ? AND status = 'ready' ORDER BY id DESC LIMIT 1"
    ))?
    .query_row(params![host_id, kind.as_str()], HostBuild::from_row)
    .optional()
}

pub struct HostBuilderDeps {
    pub db: Db,
    pub blobs: BlobStore,
    pub settings: Arc<Settings>,
    /// Told whenever what a host serves changes, so serving's cache is cleared.
    pub serving_changed: Arc<dyn Fn() + Send + Sync>,
    pub log: Logger,
    pub retry_backoff: Duration,
}

#[derive(Default)]
struct Queue {
    pending: HashSet<i64>,
    /// Whether every host is waiting, which startup or a bulk change asks for.
    everything: bool,
    pumping: bool,
    stopped: bool,
    /// Attempts spent on one build, while it is being retried.
    attempts: HashMap<String, u32>,
    retries: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct HostBuilder {
    inner: Arc<Inner>,
}

struct Inner {
    deps: HostBuilderDeps,
    queue: Mutex<Queue>,
    /// Whether the queue is being worked, for [`HostBuilder::when_idle`].
    busy: watch::Sender<bool>,
}

impl HostBuilder {
    pub fn new(deps: HostBuilderDeps) -> HostBuilder {
        HostBuilder { inner: Arc::new(Inner { deps, queue: Mutex::new(Queue::default()), busy: watch::channel(false).0 }) }
    }

    /// Clear the debris of an interrupted run and reconcile every host.
    ///
    /// A build still marked `building` can only be left over from a process
    /// that stopped mid-build; its files may be partial, and nothing would
    /// ever finish it.
    pub async fn start(&self) -> Result<()> {
        self.inner.deps.db.call(|conn| Ok(conn.execute("DELETE FROM host_builds WHERE status = 'building'", [])?)).await?;
        self.hosts_changed(None);
        Ok(())
    }

    /// Stop building. Resolves once any build in progress has settled.
    pub async fn stop(&self) {
        {
            let mut queue = self.inner.queue.lock();
            queue.stopped = true;
            for retry in queue.retries.drain(..) {
                retry.abort();
            }
        }
        self.when_idle().await;
    }

    /// Reconcile some hosts, or all of them. Returns immediately; the previous
    /// build serves until the new one is complete.
    pub fn hosts_changed(&self, host_ids: Option<&[i64]>) {
        let start = {
            let mut queue = self.inner.queue.lock();
            if queue.stopped {
                return;
            }
            match host_ids {
                Some(ids) => queue.pending.extend(ids),
                None => queue.everything = true,
            }
            !std::mem::replace(&mut queue.pumping, true)
        };

        if start {
            self.inner.busy.send_replace(true);
            let builder = self.clone();
            tokio::spawn(async move { builder.pump().await });
        }
    }

    /// Reconcile every host serving a site, because one of its versions changed.
    pub async fn site_changed(&self, address: &NsiteAddress) {
        let address = address.clone();
        let ids = self
            .inner
            .deps
            .db
            .call(move |conn| {
                Ok(conn
                    .prepare_cached("SELECT id FROM hosts WHERE pubkey = ? AND kind = ? AND identifier = ?")?
                    .query_map(params![address.pubkey, address.kind, address.identifier], |row| row.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap_or_default();

        if !ids.is_empty() {
            self.hosts_changed(Some(&ids));
        }
    }

    /// Resolves once nothing is waiting to be built.
    pub async fn when_idle(&self) {
        let mut busy = self.inner.busy.subscribe();
        let _ = busy.wait_for(|busy| !busy).await;
    }

    async fn pump(&self) {
        loop {
            let ids = {
                let mut queue = self.inner.queue.lock();
                if queue.stopped {
                    None
                } else if queue.everything {
                    queue.everything = false;
                    queue.pending.clear();
                    Some(None)
                } else if !queue.pending.is_empty() {
                    Some(Some(queue.pending.drain().collect::<Vec<_>>()))
                } else {
                    // Settled under the lock, so a trigger arriving now starts
                    // a new pump rather than finding this one still running.
                    queue.pumping = false;
                    None
                }
            };

            let ids = match ids {
                None => break,
                Some(Some(ids)) => ids,
                Some(None) => self
                    .inner
                    .deps
                    .db
                    .call(|conn| Ok(conn.prepare("SELECT id FROM hosts")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?))
                    .await
                    .unwrap_or_default(),
            };

            for id in ids {
                if self.inner.queue.lock().stopped {
                    break;
                }
                // A panic building one host must not strand the queue.
                let _ = std::panic::AssertUnwindSafe(self.reconcile(id)).catch_unwind().await;
            }
        }

        self.inner.queue.lock().pumping = false;
        self.inner.busy.send_replace(false);
    }

    /// Make one host's builds match what it should be serving. Never fails: a
    /// host whose build fails is logged and retried, and must not stop the
    /// hosts queued behind it.
    async fn reconcile(&self, host_id: i64) {
        let deps = &self.inner.deps;

        let targets = match self.targets(host_id).await {
            Ok(targets) => targets,
            Err(error) => {
                deps.log.error("build_plan_failed", with_err(fields! { "host_id" => host_id }, error));
                return;
            }
        };

        for target in targets {
            if self.inner.queue.lock().stopped {
                return;
            }

            let (id, kind) = (host_id, target.kind);
            let current = deps.db.call(move |conn| Ok(current_build(conn, id, kind)?)).await.ok().flatten();
            if current.is_some_and(|build| build.fingerprint == target.fingerprint) {
                self.inner.queue.lock().attempts.remove(&target.attempt_key());
                continue;
            }

            match self.build(&target).await {
                Ok(()) => {
                    self.inner.queue.lock().attempts.remove(&target.attempt_key());
                }
                Err(error) => self.fail(&target, error).await,
            }
        }
    }

    /// What a host's builds should be made of.
    ///
    /// A disabled host is left with whatever builds it has: it serves nothing,
    /// and re-enabling it goes straight back to a build that is still current.
    /// A host with nothing to serve loses its builds, since the last one
    /// belongs to a version it no longer points at.
    async fn targets(&self, host_id: i64) -> Result<Vec<BuildTarget>> {
        let serving_changed = self.inner.deps.serving_changed.clone();
        self.inner
            .deps
            .db
            .call(move |conn| {
                let Some(host) = host_by_id(conn, host_id)? else {
                    return Ok(Vec::new());
                };
                if host.enabled != 1 {
                    return Ok(Vec::new());
                }

                let Some(event) = active_event(conn, &host)? else {
                    if conn.execute("DELETE FROM host_builds WHERE host_id = ?", [host_id])? > 0 {
                        serving_changed();
                    }
                    return Ok(Vec::new());
                };

                let chain = version_chain(conn, &host, &event)?;
                let onion: Option<String> = conn
                    .prepare_cached("SELECT address FROM onion_services WHERE host_id = ?")?
                    .query_row([host_id], |row| row.get(0))
                    .optional()?;
                let tracker = host.analytics == 1;
                let vars = host_env(conn, host_id)?;
                let env = (!vars.is_empty()).then(|| env_script(&vars));

                let mut origins = vec![(BuildKind::Clearnet, RequestOrigin { hostname: host.hostname.clone(), scheme: "https" })];
                // The onion gets a build of its own because its generated files
                // name it: a sitemap fetched over Tor lists onion URLs.
                if let Some(onion) = onion {
                    origins.push((BuildKind::Onion, RequestOrigin { hostname: onion, scheme: "http" }));
                }

                Ok(origins
                    .into_iter()
                    .map(|(kind, origin)| {
                        let fingerprint = fingerprint(&host, &origin, &chain, tracker, env.as_deref());
                        BuildTarget {
                            host: host.clone(),
                            kind,
                            origin,
                            event: event.clone(),
                            chain: chain.clone(),
                            tracker,
                            env: env.clone(),
                            fingerprint,
                        }
                    })
                    .collect())
            })
            .await
    }

    /// Make one build and put it live, or fail leaving the old one serving.
    async fn build(&self, target: &BuildTarget) -> Result<()> {
        let deps = &self.inner.deps;
        let started = std::time::Instant::now();
        let now = crate::util::now();
        let t = target.clone();

        let id = deps
            .db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO host_builds (host_id, kind, origin, event_id, fingerprint, status, error, last_modified, created_at, built_at)
                     VALUES (?, ?, ?, ?, ?, 'building', NULL, ?, ?, NULL)",
                    params![
                        t.host.id,
                        t.kind.as_str(),
                        origin_url(&t.origin),
                        t.event.event_id,
                        t.fingerprint,
                        last_modified(&t.event),
                        now
                    ],
                )?;
                let id = conn.last_insert_rowid();
                write_deploy_log(
                    conn,
                    &t.event.event_id,
                    "info",
                    "build_started",
                    fields! { "hostname" => t.origin.hostname.as_str(), "kind" => t.kind.as_str(), "build_id" => id, "versions" => t.chain.len() },
                )?;
                Ok(id)
            })
            .await?;

        let t = target.clone();
        let blobs = deps.blobs.clone();
        let log = deps.log;
        let assembled = deps.db.call(move |conn| assemble(conn, &blobs, &t, &log)).await;

        let files = match assembled {
            Ok(files) => files,
            Err(error) => {
                let message = error.to_string();
                let _ = deps
                    .db
                    .call(move |conn| {
                        conn.execute("UPDATE host_builds SET status = 'failed', error = ? WHERE id = ?", params![message, id])?;
                        Ok(())
                    })
                    .await;
                return Err(error);
            }
        };

        let count = files.len();
        let t = target.clone();
        deps.db
            .call(move |conn| {
                let previous = current_build(conn, t.host.id, t.kind)?;
                let tx = conn.transaction()?;
                {
                    let mut insert = tx.prepare_cached(
                        "INSERT INTO host_build_files (build_id, path, sha256, event_id, source_sha256) VALUES (?, ?, ?, ?, ?)",
                    )?;
                    for (path, file) in &files {
                        insert.execute(params![id, path, file.sha256, file.event_id, file.source_sha256])?;
                    }
                }
                tx.execute("UPDATE host_builds SET status = 'ready', built_at = ? WHERE id = ?", params![crate::util::now(), id])?;
                // The build being replaced is kept one generation longer: a
                // request that looked it up a moment before this commit may
                // still be resolving a path in it.
                let keep = previous.map_or(id, |p| p.id);
                tx.execute(
                    "DELETE FROM host_builds WHERE host_id = ? AND kind = ? AND id NOT IN (?, ?)",
                    params![t.host.id, t.kind.as_str(), id, keep],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;

        (deps.serving_changed)();

        let fields = fields! {
            "hostname" => target.origin.hostname.as_str(),
            "kind" => target.kind.as_str(),
            "build_id" => id,
            "files" => count,
        };
        deps.log.info("build_ready", {
            let mut f = fields.clone();
            f.push(("ms", (started.elapsed().as_millis() as u64).into()));
            f
        });
        let event_id = target.event.event_id.clone();
        deps.db
            .call(move |conn| {
                write_deploy_log(conn, &event_id, "info", "build_ready", fields)?;
                prune_deploy_log(conn, &event_id)?;
                Ok(())
            })
            .await?;

        Ok(())
    }

    /// Record a failed build, and retry it if any attempts are left — counted
    /// per run, as deploys are.
    async fn fail(&self, target: &BuildTarget, error: anyhow::Error) {
        let deps = &self.inner.deps;
        let key = target.attempt_key();
        let limit = deps.settings.get().deploy_attempts as u32;

        let (attempt, again) = {
            let mut queue = self.inner.queue.lock();
            let attempt = queue.attempts.get(&key).copied().unwrap_or(0) + 1;
            let again = attempt < limit && !queue.stopped;
            if again {
                queue.attempts.insert(key.clone(), attempt);
            } else {
                queue.attempts.remove(&key);
            }
            (attempt, again)
        };

        let fields = fields! { "hostname" => target.origin.hostname.as_str(), "kind" => target.kind.as_str(), "attempt" => attempt };
        deps.log.error("build_failed", with_err(fields.clone(), &error));

        let (event_id, host_id, kind, message) = (target.event.event_id.clone(), target.host.id, target.kind, error.to_string());
        let _ = deps
            .db
            .call(move |conn| {
                let mut f = fields;
                f.push(("err_msg", message.into()));
                write_deploy_log(conn, &event_id, "error", "build_failed", f)?;
                // One failure explains itself; the ones before it would only repeat it.
                let latest: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM host_builds WHERE host_id = ? AND kind = ? AND status = 'failed' ORDER BY id DESC LIMIT 1",
                        params![host_id, kind.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(latest) = latest {
                    conn.execute(
                        "DELETE FROM host_builds WHERE host_id = ? AND kind = ? AND status = 'failed' AND id < ?",
                        params![host_id, kind.as_str(), latest],
                    )?;
                }
                Ok(())
            })
            .await;

        if !again {
            return;
        }

        let backoff = deps.retry_backoff * 2u32.pow(attempt - 1);
        let builder = self.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(backoff).await;
            builder.hosts_changed(Some(&[host_id]));
        });
        let mut queue = self.inner.queue.lock();
        queue.retries.retain(|retry| !retry.is_finished());
        queue.retries.push(handle);
    }
}

/// Every path the build answers, and the blob behind each. Blocking.
fn assemble(conn: &Connection, blobs: &BlobStore, target: &BuildTarget, log: &Logger) -> Result<IndexMap<String, BuildFile>> {
    let placeholders = vec!["?"; target.chain.len()].join(", ");
    let rows: Vec<(String, String, String)> = conn
        .prepare(&format!("SELECT path, sha256, event_id FROM site_files WHERE event_id IN ({placeholders})"))?
        .query_map(rusqlite::params_from_iter(&target.chain), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut files: IndexMap<String, BuildFile> = newest_by_path(rows, &target.chain, |row| (row.0.as_str(), row.2.as_str()))
        .into_iter()
        .map(|(path, (_, sha256, event_id))| (path, BuildFile { sha256, event_id: Some(event_id), source_sha256: None }))
        .collect();

    if target.host.seo != 0 {
        seo(conn, blobs, target, &mut files, log)?;
    }
    scripts(conn, blobs, target, &mut files, log)?;

    Ok(files)
}

/// A script npanel adds to every page of a build, and the file it loads.
struct Addition {
    /// Where the file is in the build.
    path: &'static str,
    body: String,
    /// What the tag loads, which may carry a version the path doesn't.
    src: String,
    integrity: String,
    module: bool,
    placement: Placement,
    connects: bool,
    /// The deploy log entry saying it was added.
    msg: &'static str,
}

/// npanel's scripts — `window.ENV` and the analytics tracker — and every page
/// of the site loading them. Each page is rewritten once per distinct blob,
/// with every script at once, and the rewritten page names the published one
/// as its source.
fn scripts(conn: &Connection, blobs: &BlobStore, target: &BuildTarget, files: &mut IndexMap<String, BuildFile>, log: &Logger) -> Result<()> {
    let integrity = |body: &str| format!("sha384-{}", base64::engine::general_purpose::STANDARD.encode(Sha384::digest(body.as_bytes())));

    let mut additions = Vec::new();
    if let Some(env) = &target.env {
        additions.push(Addition {
            path: ENV_PATH,
            src: versioned_src(ENV_PATH, env.as_bytes()),
            integrity: integrity(env),
            body: env.clone(),
            module: false,
            placement: Placement::Start,
            connects: false,
            msg: "env_added",
        });
    }
    if target.tracker {
        let body = tracker_script(&target.host.hostname);
        additions.push(Addition {
            path: TRACKER_PATH,
            src: versioned_src(TRACKER_PATH, body.as_bytes()),
            integrity: integrity(&body),
            body,
            module: true,
            placement: Placement::End,
            connects: true,
            msg: "analytics_added",
        });
    }
    if additions.is_empty() {
        return Ok(());
    }

    // Per published blob: the page it became, if any script reached it, and
    // which ones did.
    let mut rewritten: HashMap<String, (Option<String>, Vec<bool>)> = HashMap::new();
    let mut pages = vec![0; additions.len()];
    let mut skipped = vec![0; additions.len()];

    let paths: Vec<String> = files.keys().cloned().collect();
    for path in paths {
        if !matches!(extname(&path).to_lowercase().as_str(), ".html" | ".htm") {
            continue;
        }
        let file = files[&path].clone();

        if !rewritten.contains_key(&file.sha256) {
            let mut added = vec![false; additions.len()];
            let mut sha256 = None;
            if blobs.size(&file.sha256).is_some_and(|size| size <= MAX_INJECT_BYTES) {
                let mut page = blobs.read_bytes(&file.sha256, MAX_INJECT_BYTES);
                for (i, addition) in additions.iter().enumerate() {
                    let script = Script {
                        src: &addition.src,
                        integrity: &addition.integrity,
                        module: addition.module,
                        placement: addition.placement,
                        connects: addition.connects,
                    };
                    if let Some(with_script) = inject_script(&page, &script) {
                        page = with_script;
                        added[i] = true;
                    }
                }
                if added.contains(&true) {
                    sha256 = Some(store(conn, blobs, &path, &page, log)?);
                }
            }
            rewritten.insert(file.sha256.clone(), (sha256, added));
        }

        let (sha256, added) = &rewritten[&file.sha256];
        for (i, added) in added.iter().enumerate() {
            if *added {
                pages[i] += 1;
            } else {
                skipped[i] += 1;
            }
        }
        if let Some(sha256) = sha256 {
            files.insert(path, BuildFile { sha256: sha256.clone(), event_id: file.event_id, source_sha256: Some(file.sha256) });
        }
    }

    for (i, addition) in additions.iter().enumerate() {
        let sha256 = store(conn, blobs, addition.path, addition.body.as_bytes(), log)?;
        files.insert(addition.path.to_string(), BuildFile { sha256, event_id: None, source_sha256: None });

        write_deploy_log(
            conn,
            &target.event.event_id,
            "debug",
            addition.msg,
            fields! { "hostname" => target.origin.hostname.as_str(), "kind" => target.kind.as_str(), "pages" => pages[i], "skipped" => skipped[i] },
        )?;
    }
    Ok(())
}

/// The sitemap and `robots.txt` a site gets for the sake of search engines,
/// each only where the site published no file at that path — except that a
/// published `robots.txt` naming no sitemap gets npanel's line appended.
///
/// The sitemap lists the served version's own pages, even on a merged host:
/// merging keeps a withdrawn URL answering, not a reason to invite a crawler.
fn seo(conn: &Connection, blobs: &BlobStore, target: &BuildTarget, files: &mut IndexMap<String, BuildFile>, log: &Logger) -> Result<()> {
    let origin = &target.origin;

    if !files.contains_key("/sitemap.xml") {
        let xml = sitemap_xml(
            origin,
            &sitemap_paths(conn, &target.event.event_id)?,
            &version_date(target.event.created_at, target.event.fetched_at),
        );
        let sha256 = store(conn, blobs, "/sitemap.xml", xml.as_bytes(), log)?;
        files.insert("/sitemap.xml".into(), BuildFile { sha256, event_id: None, source_sha256: None });
    }

    let Some(robots) = files.get("/robots.txt").cloned() else {
        let sha256 = store(conn, blobs, "/robots.txt", robots_txt(origin).as_bytes(), log)?;
        files.insert("/robots.txt".into(), BuildFile { sha256, event_id: None, source_sha256: None });
        return Ok(());
    };

    if blobs.size(&robots.sha256).is_none_or(|size| size > MAX_ROBOTS_BYTES) {
        return Ok(());
    }
    let published = blobs.read_text(&robots.sha256, MAX_ROBOTS_BYTES);
    if !robots_needs_sitemap(&published) {
        return Ok(());
    }

    let sha256 = store(conn, blobs, "/robots.txt", augment_robots(&published, origin).as_bytes(), log)?;
    files.insert("/robots.txt".into(), BuildFile { sha256, event_id: robots.event_id, source_sha256: Some(robots.sha256) });
    Ok(())
}

/// Store a file npanel made, exactly as a downloaded one is stored. A file
/// that comes out the same as last build's is already on disk with its
/// compressed copies, and costs nothing but the hash. Compression failing is
/// never fatal: an uncompressed file is a slower site, not a broken one.
fn store(conn: &Connection, blobs: &BlobStore, path: &str, bytes: &[u8], log: &Logger) -> Result<String> {
    let sha256 = hex::encode(Sha256::digest(bytes));

    if !blobs.has(&sha256) {
        blobs.put(&sha256, bytes)?;
    }
    record_blob(conn, &sha256, bytes.len() as u64)?;

    if is_compressible(path) {
        if let Err(error) = blobs.ensure_variants(&sha256) {
            log.warn("blob_compress_failed", with_err(fields! { "sha256" => sha256.as_str() }, error));
        }
    }
    let variants = blobs.variants(&sha256).ok_or_else(|| anyhow!("{sha256} vanished after it was stored"))?;
    record_variant_sizes(conn, &sha256, variants)?;

    Ok(sha256)
}

/// A hash of everything a build's output depends on, serialized exactly as
/// the TypeScript npanel serialized it, so that its builds count as current.
///
/// Anything that affects only headers — the CSP switch, SPA mode — is read per
/// request and deliberately left out, so flipping one rebuilds nothing.
///
/// `window.ENV` is left out too unless the host has variables, which the
/// TypeScript npanel knows nothing of: a host without any keeps the
/// fingerprint it had there, and one with some gets a build of its own.
fn fingerprint(host: &Host, origin: &RequestOrigin, chain: &[String], tracker: bool, env: Option<&str>) -> String {
    let mut inputs = serde_json::json!({
        "builder": BUILDER_VERSION,
        "origin": origin_url(origin),
        "hostname": host.hostname,
        "chain": chain,
        "seo": host.seo,
        "tracker": if tracker { serde_json::Value::from(tracker_sha256()) } else { serde_json::Value::Null },
    });
    if let Some(env) = env {
        inputs["env"] = hex::encode(Sha256::digest(env.as_bytes())).into();
    }
    hex::encode(Sha256::digest(inputs.to_string().as_bytes()))
}

fn origin_url(origin: &RequestOrigin) -> String {
    format!("{}://{}", origin.scheme, origin.hostname)
}

/// `Last-Modified` for a build's responses: the served version's date, capped
/// at the moment npanel saw it, since an author can date a manifest however
/// they please.
fn last_modified(event: &SiteEvent) -> i64 {
    event.created_at.min(event.fetched_at).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::testing::*;

    fn sha(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    #[test]
    fn fingerprints_like_typescript() {
        let host = Host {
            id: 1,
            hostname: "docs.example.com".into(),
            pubkey: "a".repeat(64),
            kind: 15128,
            identifier: String::new(),
            pinned_event_id: None,
            enabled: 1,
            hidden: 0,
            redirect_www: 1,
            spa: 0,
            csp: 1,
            seo: 1,
            merge_deployments: 0,
            analytics: 0,
            created_by: String::new(),
            created_at: 0,
            updated_at: 0,
        };
        let origin = RequestOrigin { hostname: "docs.example.com".into(), scheme: "https" };
        // sha256 of {"builder":3,"origin":"https://docs.example.com","hostname":"docs.example.com","chain":["e1"],"seo":1,"tracker":null}
        assert_eq!(
            fingerprint(&host, &origin, &["e1".into()], false, None),
            sha(r#"{"builder":3,"origin":"https://docs.example.com","hostname":"docs.example.com","chain":["e1"],"seo":1,"tracker":null}"#)
        );
    }

    #[tokio::test]
    async fn builds_and_rebuilds() {
        let t = crate::db::testing::temp_db();
        let blobs = BlobStore::new(&t.dir.path().join("blobs"));
        let settings = Settings::load(&t.db, None).unwrap();
        let changed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = changed.clone();

        let page = "<html><head><title>Hi</title></head><body>hello</body></html>";
        blobs.put(&sha(page), page.as_bytes()).unwrap();
        let address = NsiteAddress { pubkey: "a".repeat(64), kind: 15128, identifier: String::new() };
        let host_id = t
            .db
            .with(|conn| {
                let id = insert_host(conn, "docs.example.com", &address);
                insert_version(conn, "e1", &address, 1000, &[("/index.html", &sha(page))]);
                Ok(id)
            })
            .unwrap();

        let builder = HostBuilder::new(HostBuilderDeps {
            db: t.db.clone(),
            blobs: blobs.clone(),
            settings,
            serving_changed: Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            log: Logger::new(crate::log::LogLevel::Error),
            retry_backoff: Duration::from_millis(1),
        });
        builder.start().await.unwrap();
        builder.when_idle().await;

        let files = |db: &Db| {
            db.with(|conn| {
                let build = current_build(conn, host_id, BuildKind::Clearnet)?.unwrap();
                let files: Vec<(String, Option<String>)> = conn
                    .prepare("SELECT path, event_id FROM host_build_files WHERE build_id = ? ORDER BY path")?
                    .query_map([build.id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok((build.id, files))
            })
            .unwrap()
        };

        let (first, list) = files(&t.db);
        let paths: Vec<&str> = list.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["/index.html", "/robots.txt", "/sitemap.xml"]);
        assert_eq!(changed.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Unchanged inputs rebuild nothing.
        builder.hosts_changed(Some(&[host_id]));
        builder.when_idle().await;
        assert_eq!(files(&t.db).0, first);

        // Analytics on: the page is rewritten and the tracker is added.
        t.db.with(|conn| Ok(conn.execute("UPDATE hosts SET analytics = 1 WHERE id = ?", [host_id])?)).unwrap();
        builder.hosts_changed(Some(&[host_id]));
        builder.when_idle().await;
        let (second, list) = files(&t.db);
        assert_ne!(second, first);
        assert!(list.iter().any(|(p, e)| p == TRACKER_PATH && e.is_none()));
        let index = t
            .db
            .with(|conn| Ok(conn.query_row("SELECT sha256, source_sha256 FROM host_build_files WHERE build_id = ? AND path = '/index.html'", [second], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?))
            .unwrap();
        assert_eq!(index.1.as_deref(), Some(sha(page).as_str()));
        let tracker = versioned_src(TRACKER_PATH, tracker_script("docs.example.com").as_bytes());
        assert!(blobs.read_text(&index.0, 10_000).contains(&format!(r#"<script type="module" src="{tracker}"></script></head>"#)));

        // A variable too: both scripts, in one rewrite of the published page.
        t.db.with(|conn| Ok(conn.execute("INSERT INTO host_env (host_id, name, value, created_at, updated_at) VALUES (?, 'A', 'b', 0, 0)", [host_id])?))
            .unwrap();
        builder.hosts_changed(Some(&[host_id]));
        builder.when_idle().await;
        let (third, list) = files(&t.db);
        assert_ne!(third, second);
        assert!(list.iter().any(|(p, e)| p == ENV_PATH && e.is_none()));
        let index = t
            .db
            .with(|conn| Ok(conn.query_row("SELECT sha256, source_sha256 FROM host_build_files WHERE build_id = ? AND path = '/index.html'", [third], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?))
            .unwrap();
        assert_eq!(index.1.as_deref(), Some(sha(page).as_str()));
        let src = versioned_src(ENV_PATH, env_script(&[("A".into(), "b".into())]).as_bytes());
        assert_eq!(
            blobs.read_text(&index.0, 10_000),
            format!(r#"<html><head><script src="{src}"></script><title>Hi</title><script type="module" src="{tracker}"></script></head><body>hello</body></html>"#)
        );

        builder.stop().await;
    }
}
