//! Host builds: the frozen bundle each host serves.
//!
//! A build is a host's version chain flattened into one path map, plus every
//! file npanel generates or rewrites for that host — a sitemap, a `robots.txt`
//! with a line appended, `window.ENV`, the analytics tracker and the error
//! reporter, and every page loading them. All
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
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::FutureExt;
use indexmap::IndexMap;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256, Sha384};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::analytics::tracker::{tracker_script, tracker_sha256, TRACKER_PATH};
use crate::blobs::BlobStore;
use crate::db::{BuildKind, Db, Host, HostBuild, SiteEvent, HOST_BUILD_COLUMNS};
use crate::errors::reporter::{reporter_script, reporter_sha256, REPORTER_PATH};
use crate::fields;
use crate::hash::BlobHash;
use crate::inject::{inject_script, versioned_src, Placement, Script, MAX_INJECT_BYTES};
use crate::link_previews::script;
use crate::log::{with_err, Logger};
use crate::mime::{is_compressible, is_html};
use crate::nsite_address::NsiteAddress;
use crate::seo::{augment_robots, robots_needs_sitemap, robots_txt, sitemap_paths, sitemap_xml, version_date, MAX_ROBOTS_BYTES};
use crate::serve::RequestOrigin;
use crate::settings::Settings;
use crate::site_env::{env_script, host_env, ENV_PATH};
use crate::sites::{
    active_event, host_by_id, newest_by_path, prune_deploy_log, record_blob, record_variant_sizes, version_chain, write_deploy_log,
};

/// Bumped whenever what a build produces changes for the same inputs. Part of
/// every fingerprint, so a new npanel whose builds would come out differently
/// rebuilds every host at startup.
///
/// 4 began editing pages at tags found the way a browser's tokenizer finds
/// them, rather than by patterns that took `</header>` for `</head>`.
///
/// 5 moved npanel's files from `/.npanel/` to `/_npanel/`, out of the way of
/// proxies that refuse to pass dotfiles. Paths aren't otherwise in the
/// fingerprint, so without it a tracker built before would go on reporting to
/// an endpoint that no longer answers.
///
/// 6 loads the tracker `async`, so a request for it that never returns holds
/// up nothing the site wrote.
pub const BUILDER_VERSION: i64 = 6;

/// Wait before a second attempt at a build. It doubles after that.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// One path of a build, as it goes into `host_build_files`.
#[derive(Clone, Debug)]
pub struct BuildFile {
    pub sha256: BlobHash,
    /// The version that published it, or `None` for a file npanel generated.
    pub event_id: Option<String>,
    /// The published blob it was rewritten from, if it was.
    pub source_sha256: Option<BlobHash>,
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
    /// Whether its pages load the error reporter.
    reporter: bool,
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

enum Command {
    /// Reconcile these hosts, or every host. Carries the request's number, so
    /// [`HostBuilder::when_idle`] can tell when it has been worked through.
    Reconcile(Option<Vec<i64>>, u64),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct HostBuilder {
    inner: Arc<Inner>,
}

struct Inner {
    deps: HostBuilderDeps,
    commands: mpsc::UnboundedSender<Command>,
    /// The number of the latest request, and of the latest one worked through.
    requested: watch::Sender<u64>,
    completed: watch::Sender<u64>,
}

impl HostBuilder {
    /// A builder, with the task that works its queue.
    pub fn new(deps: HostBuilderDeps) -> HostBuilder {
        let (commands, received) = mpsc::unbounded_channel();
        let inner = Inner { deps, commands, requested: watch::Sender::new(0), completed: watch::Sender::new(0) };
        let builder = HostBuilder { inner: Arc::new(inner) };
        tokio::spawn(Queue::new(builder.clone(), received).run());
        builder
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

    /// Stop building. Resolves once any build in progress has settled; retries
    /// still waiting are dropped, and anything asked after is ignored.
    pub async fn stop(&self) {
        let (done, stopped) = oneshot::channel();
        if self.inner.commands.send(Command::Stop(done)).is_ok() {
            let _ = stopped.await;
        }
    }

    /// Reconcile some hosts, or all of them. Returns immediately; the previous
    /// build serves until the new one is complete.
    pub fn hosts_changed(&self, host_ids: Option<&[i64]>) {
        let mut number = 0;
        self.inner.requested.send_modify(|requested| {
            *requested += 1;
            number = *requested;
        });
        let _ = self.inner.commands.send(Command::Reconcile(host_ids.map(<[i64]>::to_vec), number));
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
            .unwrap_or_else(|error| {
                self.inner.deps.log.error("build_lookup_failed", with_err(fields! {}, error));
                Vec::new()
            });

        if !ids.is_empty() {
            self.hosts_changed(Some(&ids));
        }
    }

    /// Resolves once everything asked of the builder so far has been worked
    /// through — not the retries that may follow, which wait out a backoff.
    #[cfg(test)]
    pub async fn when_idle(&self) {
        let asked = *self.inner.requested.borrow();
        let mut completed = self.inner.completed.subscribe();
        // An error means the queue is gone, so there is nothing to wait for.
        let _ = completed.wait_for(|completed| *completed >= asked).await;
    }
}

/// The builder's queue, and the one task that works it. Owning its state
/// outright is what lets a request never be lost between checking the queue
/// and going to sleep: both happen here, one after the other.
struct Queue {
    builder: HostBuilder,
    commands: mpsc::UnboundedReceiver<Command>,
    pending: HashSet<i64>,
    /// Whether every host is waiting, which startup or a bulk change asks for.
    everything: bool,
    /// The number of the latest request taken off the channel.
    absorbed: u64,
    /// Attempts spent on one build, while it is being retried.
    attempts: HashMap<String, u32>,
    /// Hosts to reconcile again once their backoff is over.
    retries: Vec<(Instant, i64)>,
    stopping: Option<oneshot::Sender<()>>,
}

impl Queue {
    fn new(builder: HostBuilder, commands: mpsc::UnboundedReceiver<Command>) -> Queue {
        Queue {
            builder,
            commands,
            pending: HashSet::new(),
            everything: false,
            absorbed: 0,
            attempts: HashMap::new(),
            retries: Vec::new(),
            stopping: None,
        }
    }

    fn deps(&self) -> &HostBuilderDeps {
        &self.builder.inner.deps
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Reconcile(Some(ids), number) => {
                self.pending.extend(ids);
                self.absorbed = self.absorbed.max(number);
            }
            Command::Reconcile(None, number) => {
                self.everything = true;
                self.absorbed = self.absorbed.max(number);
            }
            Command::Stop(done) => self.stopping = Some(done),
        }
    }

    /// Take whatever has been asked since last looked. Whether to go on.
    fn absorb(&mut self) -> bool {
        while let Ok(command) = self.commands.try_recv() {
            self.apply(command);
        }
        self.stopping.is_none()
    }

    async fn run(mut self) {
        loop {
            if !self.absorb() {
                break;
            }
            let now = Instant::now();
            let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retries).into_iter().partition(|(at, _)| *at <= now);
            self.retries = waiting;
            self.pending.extend(due.into_iter().map(|(_, id)| id));

            let ids = if std::mem::take(&mut self.everything) {
                self.pending.clear();
                let all = self
                    .deps()
                    .db
                    .call(|conn| Ok(conn.prepare("SELECT id FROM hosts")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?))
                    .await;
                all.unwrap_or_else(|error| {
                    self.deps().log.error("build_lookup_failed", with_err(fields! {}, error));
                    Vec::new()
                })
            } else {
                self.pending.drain().collect()
            };

            if ids.is_empty() {
                self.builder.inner.completed.send_replace(self.absorbed);
                let soonest = self.retries.iter().map(|(at, _)| *at).min();
                tokio::select! {
                    command = self.commands.recv() => match command {
                        Some(command) => self.apply(command),
                        None => break,
                    },
                    _ = sleep_until(soonest) => {}
                }
                continue;
            }

            for id in ids {
                if !self.absorb() {
                    break;
                }
                // A panic building one host must not strand the queue.
                if AssertUnwindSafe(self.reconcile(id)).catch_unwind().await.is_err() {
                    self.deps().log.error("build_panicked", fields! { "host_id" => id });
                }
            }
        }

        self.builder.inner.completed.send_replace(self.absorbed);
        if let Some(done) = self.stopping.take() {
            let _ = done.send(());
        }
    }

    /// Make one host's builds match what it should be serving. Never fails: a
    /// host whose build fails is logged and retried, and must not stop the
    /// hosts queued behind it.
    async fn reconcile(&mut self, host_id: i64) {
        let targets = match self.builder.targets(host_id).await {
            Ok(targets) => targets,
            Err(error) => {
                self.deps().log.error("build_plan_failed", with_err(fields! { "host_id" => host_id }, error));
                return;
            }
        };

        for target in targets {
            if !self.absorb() {
                return;
            }

            let (id, kind) = (host_id, target.kind);
            let current = match self.deps().db.call(move |conn| Ok(current_build(conn, id, kind)?)).await {
                Ok(current) => current,
                Err(error) => {
                    self.deps().log.error("build_plan_failed", with_err(fields! { "host_id" => host_id }, error));
                    continue;
                }
            };
            if current.is_some_and(|build| build.fingerprint == target.fingerprint) {
                self.attempts.remove(&target.attempt_key());
                continue;
            }

            match self.builder.build(&target).await {
                Ok(()) => {
                    self.attempts.remove(&target.attempt_key());
                }
                Err(error) => self.fail(&target, error).await,
            }
        }
    }

    /// Record a failed build, and retry it if any attempts are left — counted
    /// per run, as deploys are.
    async fn fail(&mut self, target: &BuildTarget, error: anyhow::Error) {
        let key = target.attempt_key();
        let limit = self.deps().settings.get().deploy_attempts as u32;

        let attempt = self.attempts.get(&key).copied().unwrap_or(0) + 1;
        let again = attempt < limit && self.stopping.is_none();
        if again {
            self.attempts.insert(key, attempt);
        } else {
            self.attempts.remove(&key);
        }

        let deps = self.deps();
        let fields = fields! { "hostname" => target.origin.hostname.as_str(), "kind" => target.kind.as_str(), "attempt" => attempt };
        deps.log.error("build_failed", with_err(fields.clone(), &error));

        let (event_id, host_id, kind, message) = (target.event.event_id.clone(), target.host.id, target.kind, error.to_string());
        let recorded = deps
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
        if let Err(error) = recorded {
            deps.log.error("build_record_failed", with_err(fields! { "host_id" => host_id }, error));
        }

        if again {
            let backoff = deps.retry_backoff * 2u32.pow(attempt - 1);
            self.retries.push((Instant::now() + backoff, host_id));
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl HostBuilder {
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
                if !host.enabled {
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
                // A host counting on the server has nothing to add to its pages.
                let tracker = host.analytics && host.analytics_script;
                let reporter = host.errors;
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
                        let fingerprint = fingerprint(&host, &origin, &chain, tracker, reporter, env.as_deref());
                        BuildTarget {
                            host: host.clone(),
                            kind,
                            origin,
                            event: event.clone(),
                            chain: chain.clone(),
                            tracker,
                            reporter,
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
                        t.event.last_modified(),
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

        // Once per version: the onion's build has the same script. Beside
        // the build rather than in its way: what it says is for the deploy
        // log, and a site goes live without waiting for a stranger's code.
        if let Some(sha256) = files.get(script::PATH).map(|file| file.sha256).filter(|_| target.kind == BuildKind::Clearnet) {
            let (builder, target) = (self.clone(), target.clone());
            tokio::spawn(async move { builder.check_script(&target, sha256).await });
        }

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
}

impl HostBuilder {
    /// The site's own link-preview script, loaded and checked as a run would
    /// load it, and what's wrong with it said in its deploy log. A script that
    /// fails leaves the site without previews, and nothing else. A site with
    /// link previews off has its script left alone: it is the author's code,
    /// and nothing runs it until staff say it may.
    async fn check_script(&self, target: &BuildTarget, sha256: BlobHash) {
        let deps = &self.inner.deps;
        let hostname = target.origin.hostname.as_str();
        let mut entries: Vec<(&'static str, &'static str, crate::log::Fields)> = Vec::new();
        if target.host.link_previews {
            let blobs = deps.blobs.clone();
            let Ok(loaded) = tokio::task::spawn_blocking(move || script::load(sha256, &blobs)).await else { return };
            match loaded.map(Arc::new) {
                Ok(found) => match script::check(found).await {
                    Ok(()) => entries.push(("info", "preview_script_found", fields! { "hostname" => hostname })),
                    Err(error) => entries.push(("warn", "preview_script_failed", fields! { "hostname" => hostname, "err_msg" => error })),
                },
                Err(error) => entries.push(("warn", "preview_script_failed", fields! { "hostname" => hostname, "err_msg" => error })),
            }
        } else {
            let message = "Link previews are off for this site, so its preview script isn't run. Turn them on in the site's SEO settings.";
            entries.push(("warn", "preview_script_unused", fields! { "hostname" => hostname, "err_msg" => message }));
        }

        let event_id = target.event.event_id.clone();
        let written = deps
            .db
            .call(move |conn| {
                for (level, msg, fields) in entries {
                    write_deploy_log(conn, &event_id, level, msg, fields)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = written {
            deps.log.error("build_record_failed", with_err(fields! { "host_id" => target.host.id }, error));
        }
    }
}

/// Every path the build answers, and the blob behind each. Blocking.
fn assemble(conn: &Connection, blobs: &BlobStore, target: &BuildTarget, log: &Logger) -> Result<IndexMap<String, BuildFile>> {
    let placeholders = vec!["?"; target.chain.len()].join(", ");
    let rows: Vec<(String, BlobHash, String)> = conn
        .prepare(&format!("SELECT path, sha256, event_id FROM site_files WHERE event_id IN ({placeholders})"))?
        .query_map(rusqlite::params_from_iter(&target.chain), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut files: IndexMap<String, BuildFile> = newest_by_path(rows, &target.chain, |row| (row.0.as_str(), row.2.as_str()))
        .into_iter()
        .map(|(path, (_, sha256, event_id))| (path, BuildFile { sha256, event_id: Some(event_id), source_sha256: None }))
        .collect();

    if target.host.seo {
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
    asynchronous: bool,
    placement: Placement,
    connects: bool,
    /// The deploy log entry saying it was added.
    msg: &'static str,
}

/// npanel's scripts — `window.ENV`, the analytics tracker and the error
/// reporter — and every page
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
            asynchronous: false,
            placement: Placement::Start,
            connects: false,
            msg: "env_added",
        });
    }
    if target.reporter {
        let body = reporter_script(&target.event.event_id);
        additions.push(Addition {
            path: REPORTER_PATH,
            src: versioned_src(REPORTER_PATH, body.as_bytes()),
            integrity: integrity(&body),
            body,
            module: false,
            // It has to be listening before anything the site wrote runs, or
            // an error while the page loads goes unseen. Small for that
            // reason, as `window.ENV` is.
            asynchronous: false,
            placement: Placement::Start,
            connects: true,
            msg: "errors_added",
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
            // Nothing waits on a page view being counted, and nothing should:
            // on a dead connection a deferred tracker would hold back every
            // deferred script after it, the site's own included.
            asynchronous: true,
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
    let mut rewritten: HashMap<BlobHash, (Option<BlobHash>, Vec<bool>)> = HashMap::new();
    let mut pages = vec![0; additions.len()];
    let mut skipped = vec![0; additions.len()];

    let paths: Vec<String> = files.keys().cloned().collect();
    for path in paths {
        if !is_html(&path) {
            continue;
        }
        let file = files[&path].clone();

        if let std::collections::hash_map::Entry::Vacant(slot) = rewritten.entry(file.sha256) {
            let mut added = vec![false; additions.len()];
            let mut sha256 = None;
            if blobs.size(file.sha256).is_some_and(|size| size <= MAX_INJECT_BYTES) {
                let mut page = blobs.read_bytes(file.sha256, MAX_INJECT_BYTES);
                for (i, addition) in additions.iter().enumerate() {
                    let script = Script {
                        src: &addition.src,
                        integrity: &addition.integrity,
                        module: addition.module,
                        asynchronous: addition.asynchronous,
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
            slot.insert((sha256, added));
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
            files.insert(path, BuildFile { sha256: *sha256, event_id: file.event_id, source_sha256: Some(file.sha256) });
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
            &version_date(target.event.last_modified()),
        );
        let sha256 = store(conn, blobs, "/sitemap.xml", xml.as_bytes(), log)?;
        files.insert("/sitemap.xml".into(), BuildFile { sha256, event_id: None, source_sha256: None });
    }

    let Some(robots) = files.get("/robots.txt").cloned() else {
        let sha256 = store(conn, blobs, "/robots.txt", robots_txt(origin).as_bytes(), log)?;
        files.insert("/robots.txt".into(), BuildFile { sha256, event_id: None, source_sha256: None });
        return Ok(());
    };

    if blobs.size(robots.sha256).is_none_or(|size| size > MAX_ROBOTS_BYTES) {
        return Ok(());
    }
    let published = blobs.read_text(robots.sha256, MAX_ROBOTS_BYTES);
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
fn store(conn: &Connection, blobs: &BlobStore, path: &str, bytes: &[u8], log: &Logger) -> Result<BlobHash> {
    let sha256 = BlobHash::of(bytes);

    if !blobs.has(sha256) {
        blobs.put(sha256, bytes)?;
    }
    record_blob(conn, sha256, bytes.len() as u64)?;

    if is_compressible(path) {
        if let Err(error) = blobs.ensure_variants(sha256) {
            log.warn("blob_compress_failed", with_err(fields! { "sha256" => sha256 }, error));
        }
    }
    let variants = blobs.variants(sha256).ok_or_else(|| anyhow!("{sha256} vanished after it was stored"))?;
    record_variant_sizes(conn, sha256, variants)?;

    Ok(sha256)
}

/// A hash of everything a build's output depends on. Changing how it is
/// serialized changes every fingerprint and rebuilds every host, so the
/// serialization is pinned by a test.
///
/// Anything that affects only headers — the CSP switch, SPA mode — is read per
/// request and deliberately left out, so flipping one rebuilds nothing.
///
/// `window.ENV` is left out too unless the host has variables, so adding them
/// to npanel didn't rebuild the hosts that have none, and the error reporter
/// unless it is on, for the same reason.
fn fingerprint(host: &Host, origin: &RequestOrigin, chain: &[String], tracker: bool, reporter: bool, env: Option<&str>) -> String {
    // A struct rather than `json!`, so the key order is fixed by the
    // declaration and not by a crate-wide serde_json feature.
    #[derive(serde::Serialize)]
    struct Inputs<'a> {
        builder: i64,
        origin: String,
        hostname: &'a str,
        chain: &'a [String],
        /// 0 or 1, as stored in the table.
        seo: i64,
        tracker: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reporter: Option<&'static str>,
    }
    let inputs = Inputs {
        builder: BUILDER_VERSION,
        origin: origin_url(origin),
        hostname: &host.hostname,
        chain,
        seo: i64::from(host.seo),
        tracker: tracker.then(tracker_sha256),
        env: env.map(|env| hex::encode(Sha256::digest(env.as_bytes()))),
        reporter: reporter.then(reporter_sha256),
    };
    let serialized = serde_json::to_string(&inputs).expect("serializable");
    hex::encode(Sha256::digest(serialized.as_bytes()))
}

fn origin_url(origin: &RequestOrigin) -> String {
    format!("{}://{}", origin.scheme, origin.hostname)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::testing::*;

    fn sha(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    #[test]
    fn fingerprint_serialization_is_stable() {
        let host = Host {
            id: 1,
            hostname: "docs.example.com".into(),
            pubkey: "a".repeat(64),
            kind: 15128,
            identifier: String::new(),
            pinned_event_id: None,
            enabled: true,
            hidden: false,
            redirect_www: true,
            spa: false,
            csp: true,
            seo: true,
            merge_deployments: false,
            analytics: false,
            analytics_script: true,
            link_previews: false,
            errors: false,
            created_by: String::new(),
            created_at: 0,
            updated_at: 0,
        };
        let origin = RequestOrigin { hostname: "docs.example.com".into(), scheme: "https" };
        // Down to the key order: any change rebuilds every host.
        assert_eq!(
            fingerprint(&host, &origin, &["e1".into()], false, false, None),
            sha(&format!(
                r#"{{"builder":{BUILDER_VERSION},"origin":"https://docs.example.com","hostname":"docs.example.com","chain":["e1"],"seo":1,"tracker":null}}"#
            ))
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
        blobs.put(BlobHash::of(page.as_bytes()), page.as_bytes()).unwrap();
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
        assert!(blobs.read_text(BlobHash::parse(&index.0).unwrap(), 10_000).contains(&format!(r#"<script type="module" async src="{tracker}"></script></head>"#)));

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
            blobs.read_text(BlobHash::parse(&index.0).unwrap(), 10_000),
            format!(r#"<html><head><script src="{src}"></script><title>Hi</title><script type="module" async src="{tracker}"></script></head><body>hello</body></html>"#)
        );

        // Error reporting on: the reporter goes first of all, naming the
        // version it was built from.
        t.db.with(|conn| Ok(conn.execute("UPDATE hosts SET errors = 1 WHERE id = ?", [host_id])?)).unwrap();
        builder.hosts_changed(Some(&[host_id]));
        builder.when_idle().await;
        let (fourth, list) = files(&t.db);
        assert_ne!(fourth, third);
        assert!(list.iter().any(|(p, e)| p == REPORTER_PATH && e.is_none()));
        let index = t
            .db
            .with(|conn| Ok(conn.query_row("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = '/index.html'", [fourth], |r| r.get::<_, String>(0))?))
            .unwrap();
        let reporter = versioned_src(REPORTER_PATH, reporter_script("e1").as_bytes());
        assert!(blobs.read_text(BlobHash::parse(&index).unwrap(), 10_000).starts_with(&format!(r#"<html><head><script src="{reporter}"></script><script src="{src}">"#)));

        // Counting on the server instead: still counting, but no tracker.
        t.db.with(|conn| Ok(conn.execute("UPDATE hosts SET analytics_script = 0 WHERE id = ?", [host_id])?)).unwrap();
        builder.hosts_changed(Some(&[host_id]));
        builder.when_idle().await;
        let (fifth, list) = files(&t.db);
        assert_ne!(fifth, fourth);
        assert!(!list.iter().any(|(p, _)| p == TRACKER_PATH));
        let index = t
            .db
            .with(|conn| Ok(conn.query_row("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = '/index.html'", [fifth], |r| r.get::<_, String>(0))?))
            .unwrap();
        assert!(!blobs.read_text(BlobHash::parse(&index).unwrap(), 10_000).contains(TRACKER_PATH));

        builder.stop().await;
    }
}
