//! Link previews a site draws for itself, with a script.
//!
//! A site publishes one self-contained module at
//! `/.well-known/npanel/preview.js`, served like any other file, which exports
//! `default { routes, preview(request, ctx) }`. `routes`, URLPattern
//! pathnames, says which pages it previews: every HTML page, without it.
//! Given the crawler's request and a read-only Nostr relay, `preview` returns
//! the page's title, description and article, and its image — a URL, or bytes
//! it drew on an `OffscreenCanvas`, or a promise of them — or `null`, which
//! leaves the page as published. It draws text in the fonts installed on the
//! system, and imports nothing: what it needs is bundled into it.
//!
//! A script is a stranger's code, so it runs in a process of its own, which
//! may do next to nothing (`sandbox.rs`), with a QuickJS runtime of its own
//! for each run (`worker.rs`). Everything it asks of the world is asked of
//! the gateway, here: relays through `nostr`, and an `https://`-only `fetch`
//! through the public-addresses-only resolver, both metered. What it returns
//! is held to npanel's limits here too, since nothing the process says can
//! be taken on trust.
//!
//! The image is the slow part, so a run hands back the page's fields as soon
//! as the handler returns them, and goes on drawing behind them. What names
//! the image — its version — is a hash of everything the handler was given
//! until then: the script, the URL, the request headers it read, and what its
//! queries and fetches asked for and found. So it is known before the image
//! is, and changes when anything the image is drawn from changes.

pub mod canvas;
mod sandbox;
mod worker;

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::future::{BoxFuture, FutureExt, Shared};
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use self::sandbox::{FromWorker, Job, Lease, Picture, ToWorker};
use super::preview::Extras;
use super::relays::{outbox, EventSource};
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::fields;
use crate::hash::BlobHash;
use crate::log::Logger;
use crate::net::is_public_address;
use crate::nostr::pool::Filter;
use crate::nostr::service::dedupe;
use crate::nostr::{is_hex64, Event};

pub use self::sandbox::{init as init_sandbox, COMMAND as WORKER_COMMAND};
pub use self::worker::main as worker_main;

/// Where a site publishes its script.
pub const PATH: &str = "/.well-known/npanel/preview.js";

/// The request headers a script is given. Anything else a crawler sent is
/// no business of a preview's — and a script can `fetch` anywhere, so a
/// header naming the visitor, from whatever proxy is in front, would be
/// theirs to send on.
pub const HEADERS: &[&str] = &["accept", "accept-language", "user-agent"];

const MAX_SCRIPT_BYTES: u64 = 4 << 20;
/// How long a script's top level has to run when it's loaded to read its
/// routes: a crawler's request waits on it, the first time its build is
/// served.
const LOAD_DEADLINE: Duration = Duration::from_secs(2);
/// Routes a script may declare.
const MAX_ROUTES: usize = 32;

/// How long a handler has to return. Its image has as long again.
pub const DEADLINE: Duration = Duration::from_secs(10);
/// How long past a deadline the gateway waits before killing the process:
/// the worker's own deadline comes first, and says what was running.
const GRACE: Duration = Duration::from_secs(1);
/// How long a new process has to start and say it's ready: reading the
/// system's fonts, on a system with thousands, takes a while.
const STARTUP: Duration = Duration::from_secs(30);

/// What one run may ask of relays: calls, and events found in all.
const MAX_QUERIES: usize = 8;
pub const MAX_EVENTS: usize = 50;
/// The most values in one of a filter's lists, and the longest tag value.
const MAX_VALUES: usize = 100;
const MAX_TAG_VALUE: usize = 1024;
/// The authors whose outboxes are asked, when nothing was found elsewhere.
const MAX_OUTBOXES: usize = 5;

/// What one run may fetch: requests, bytes each, and bytes in all.
const MAX_FETCHES: usize = 16;
const MAX_FETCH_BYTES: usize = 8 << 20;
const MAX_FETCHED_BYTES: u64 = 32 << 20;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Lines a run may log, and the longest one.
const MAX_LOG_LINES: usize = 50;
const MAX_LOG_LINE: usize = 1024;

const STOPPED: &str = "The preview script stopped without an answer.";
const PAST_DEADLINE: &str = "The preview script ran past its deadline.";
const DEPLOYING: &str = "Nothing is fetched while a site deploys.";
const IMAGE_FORMAT: &str = "The image must be a PNG, JPEG, GIF or WebP, 4096 pixels a side at most.";

/// A build's script.
pub struct Script {
    /// Names what the script is. Two builds with the same script share
    /// previews.
    pub digest: String,
    source: String,
    /// The pages it previews, once a load has read them: `None` for every
    /// page.
    routes: OnceLock<Option<Routes>>,
}

impl Script {
    /// Whether the script previews the page at `path`, as far as its routes
    /// are known.
    pub fn serves(&self, path: &str) -> bool {
        match self.routes.get() {
            Some(Some(routes)) => routes.matches(path),
            Some(None) => true,
            None => false,
        }
    }

    fn new(source: String, digest: String) -> Script {
        Script { digest, source, routes: OnceLock::new() }
    }
}

/// Read a build's script. Blocking.
pub fn load(sha256: BlobHash, blobs: &BlobStore) -> Result<Script, String> {
    let size = blobs.size(sha256).ok_or("The preview script is missing from local storage.")?;
    if size > MAX_SCRIPT_BYTES {
        return Err("The preview script is larger than 4 MB, so it isn't loaded.".into());
    }
    let source = String::from_utf8(blobs.read_bytes(sha256, MAX_SCRIPT_BYTES)).map_err(|_| "The preview script isn't UTF-8 text, so it isn't loaded.")?;
    Ok(Script::new(source, sha256.to_string()[..32].to_string()))
}

/// The script in a build, if it has one.
pub fn build_script(conn: &Connection, build_id: i64) -> rusqlite::Result<Option<BlobHash>> {
    conn.prepare_cached("SELECT sha256 FROM host_build_files WHERE build_id = ? AND path = ?")?
        .query_row(rusqlite::params![build_id, PATH], |row| row.get(0))
        .optional()
}

/// Start a worker, so the first crawler doesn't wait for one, and so the
/// log says early what it found: the fonts it draws with, and how it's
/// confined.
pub async fn warm() {
    let _ = check(Arc::new(Script::new("export default { preview() { return null; } };".into(), "warm".into()))).await;
}

/// The pages a script previews: URLPattern pathnames.
pub struct Routes(Vec<urlpattern::UrlPattern<Bounded>>);

impl Routes {
    /// A script's `routes`, compiled, or what's wrong with them.
    pub fn compile(patterns: &[String]) -> Result<Routes, String> {
        if patterns.len() > MAX_ROUTES {
            return Err(format!("A preview script may declare {MAX_ROUTES} routes at most."));
        }
        patterns
            .iter()
            .map(|pattern| {
                let init = urlpattern::UrlPatternInit { pathname: Some(pattern.clone()), ..Default::default() };
                urlpattern::UrlPattern::parse(init, Default::default()).map_err(|e| format!("The route {pattern} isn't a URLPattern pathname npanel can use: {e}."))
            })
            .collect::<Result<_, _>>()
            .map(Routes)
    }

    pub fn matches(&self, path: &str) -> bool {
        let input = || urlpattern::UrlPatternMatchInput::Init(urlpattern::UrlPatternInit { pathname: Some(path.to_string()), ..Default::default() });
        self.0.iter().any(|pattern| pattern.test(input()).unwrap_or(false))
    }
}

/// A route's regular expressions, bounded in size like the crawler pattern,
/// so a site's routes can't cost much to match. Rust's `regex` matches in
/// linear time.
pub struct Bounded(regex::Regex);

impl urlpattern::regexp::RegExp for Bounded {
    fn syntax() -> urlpattern::RegexSyntax {
        urlpattern::RegexSyntax::Rust
    }

    fn parse(pattern: &str, flags: &str, _: bool) -> Result<Self, ()> {
        regex::RegexBuilder::new(pattern).case_insensitive(flags.contains('i')).size_limit(1 << 20).build().map(Bounded).map_err(|_| ())
    }

    fn matches<'a>(&self, text: &'a str) -> Option<Vec<Option<&'a str>>> {
        let captures = self.0.captures(text)?;
        Some(captures.iter().skip(1).map(|c| c.map(|m| m.as_str())).collect())
    }

    fn pattern_string(&self) -> &str {
        self.0.as_str()
    }
}

/// Builds' scripts, read once each, and loaded to read their routes. A
/// build's files never change, so neither does its script; and a script is
/// named by its hash, so builds that share one load it once.
pub struct Scripts {
    db: Db,
    blobs: BlobStore,
    /// Each build's script, by hash, or `None` for none.
    builds: Mutex<lru::LruCache<i64, Option<BlobHash>>>,
    loaded: Mutex<lru::LruCache<BlobHash, Loading>>,
}

/// A script being loaded, or loaded: every request for it waits on one load,
/// which goes on whether or not anyone still waits.
#[derive(Clone)]
struct Loading {
    started: Instant,
    script: Shared<BoxFuture<'static, Option<Arc<Script>>>>,
}

/// Builds whose scripts are remembered, and scripts kept loaded.
const MAX_CACHED_BUILDS: usize = 4096;
const MAX_CACHED_SCRIPTS: usize = 256;
/// How long a script that didn't load is believed not to. It is the same
/// script next time, but what stopped it — no process to run it in, say —
/// may not be.
const RELOAD_AFTER: Duration = Duration::from_secs(60);

impl Scripts {
    pub fn new(db: Db, blobs: BlobStore) -> Scripts {
        Scripts {
            db,
            blobs,
            builds: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_BUILDS).unwrap())),
            loaded: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_SCRIPTS).unwrap())),
        }
    }

    /// A build's script, or `None` for a build without one, or with one that
    /// doesn't load: its deploy log says why.
    pub async fn for_build(&self, build_id: i64) -> anyhow::Result<Option<Arc<Script>>> {
        let cached = self.builds.lock().get(&build_id).copied();
        let sha256 = match cached {
            Some(sha256) => sha256,
            None => {
                let sha256 = self.db.call(move |conn| Ok(build_script(conn, build_id)?)).await?;
                self.builds.lock().put(build_id, sha256);
                sha256
            }
        };
        let Some(sha256) = sha256 else { return Ok(None) };
        let loading = self.loading(sha256);
        Ok(loading.await)
    }

    /// A build's script without waiting for anything: `None` while that
    /// isn't known yet.
    pub fn known(&self, build_id: i64) -> Option<Option<Arc<Script>>> {
        let sha256 = (*self.builds.lock().get(&build_id)?)?;
        self.loaded.lock().get(&sha256)?.script.peek().cloned()
    }

    fn loading(&self, sha256: BlobHash) -> Shared<BoxFuture<'static, Option<Arc<Script>>>> {
        let mut loaded = self.loaded.lock();
        if let Some(loading) = loaded.get(&sha256) {
            let failed = matches!(loading.script.peek(), Some(None));
            if !failed || loading.started.elapsed() < RELOAD_AFTER {
                return loading.script.clone();
            }
        }
        let blobs = self.blobs.clone();
        let task = tokio::spawn(async move {
            let script = Arc::new(tokio::task::spawn_blocking(move || load(sha256, &blobs)).await.ok()?.ok()?);
            check(script.clone()).await.ok()?;
            Some(script)
        });
        let script = async move { task.await.ok().flatten() }.boxed().shared();
        loaded.put(sha256, Loading { started: Instant::now(), script: script.clone() });
        script
    }
}

/// The request a handler is given.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    /// The page's URL: its origin and path, without the query a crawler may
    /// have added.
    pub url: String,
    pub method: String,
    /// Of [`HEADERS`] only.
    pub headers: Vec<(String, String)>,
}

/// What a run needs from the gateway.
pub struct Env {
    /// The previews runtime, where a run is driven, and its queries and
    /// fetches made.
    pub runtime: tokio::runtime::Handle,
    pub source: Arc<dyn EventSource>,
    pub http: reqwest::Client,
    /// Relays to ask besides the configured ones: the page's own hints.
    pub relays: Vec<String>,
    pub log: Logger,
    /// Whether an image of this version is drawn already, which spares
    /// drawing it again.
    pub have_image: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// When the run happens, in milliseconds, as far as `Date` can tell.
    pub now: i64,
    /// How long the handler has to return: [`DEADLINE`], but for tests.
    pub deadline: Duration,
}

/// What a run came to.
#[derive(Debug)]
pub struct Ran {
    /// What the handler returned, or `None` for nothing: the page as published.
    pub preview: Option<Returned>,
    /// The request headers it read, by name, which the preview varies by.
    pub vary: Vec<String>,
    /// What names this preview's image.
    pub version: String,
    pub timings: Timings,
}

/// How long a run took getting to its answer, a phase at a time.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Timings {
    /// From being asked for to the job reaching a worker: waiting for one,
    /// or starting one.
    pub spawn: Duration,
    /// Making a QuickJS runtime and context.
    pub setup: Duration,
    /// Running the prelude: the web the script runs in.
    pub prelude: Duration,
    /// Loading the site's script — its bytecode, once a first load has
    /// parsed it — and running its top level.
    pub module: Duration,
    /// The handler, from its call to its return, its queries and fetches
    /// included.
    pub handler: Duration,
    /// What the JavaScript heap held when the handler returned, in bytes.
    pub heap: u64,
}

/// What a handler returned, as it wrote it: not yet held to anything.
#[derive(Debug)]
pub struct Returned {
    pub title: Option<String>,
    pub description: Option<String>,
    pub body: Option<String>,
    pub image: Image,
    pub extras: Extras,
}

#[derive(Debug)]
pub enum Image {
    None,
    Url(String),
    Bytes(Vec<u8>),
    /// Still being drawn. `None` if drawing it failed.
    Pending(oneshot::Receiver<Option<Vec<u8>>>),
}

/// Run a site's handler for one page, waiting for one of the processes
/// that may run at once.
pub async fn run(script: Arc<Script>, page: Page, env: Env) -> Result<Ran, String> {
    let (done, ran) = oneshot::channel();
    let runtime = env.runtime.clone();
    // Driven by a task of its own, since it goes on drawing after it has
    // answered.
    runtime.spawn(drive(script, page, env, Instant::now(), done));
    ran.await.unwrap_or_else(|_| Err(STOPPED.into()))
}

/// Load a site's script as a run would, check it exports a handler, without
/// calling it, and read its routes. What's wrong is for its deploy log.
pub async fn check(script: Arc<Script>) -> Result<(), String> {
    let routes = tokio::spawn(read_routes(script.clone())).await.unwrap_or_else(|_| Err(STOPPED.into()))?;
    let routes = routes.as_deref().map(Routes::compile).transpose()?;
    let _ = script.routes.set(routes);
    Ok(())
}

async fn read_routes(script: Arc<Script>) -> Result<Option<Vec<String>>, String> {
    let job = Job { script: script.digest.clone(), page: None, now: 0, deadline: LOAD_DEADLINE };
    let mut session = Session::open(script, job, None).await?;
    let answer = match session.next().await? {
        (FromWorker::Loaded { routes }, _) => Ok(routes),
        (FromWorker::Failed(error), _) => Err(error),
        _ => return Err(confused()),
    };
    session.close();
    answer
}

/// One run, start to finish: its fields, then its image.
async fn drive(script: Arc<Script>, page: Page, env: Env, asked: Instant, done: oneshot::Sender<Result<Ran, String>>) {
    let world = World {
        source: env.source.clone(),
        http: env.http.clone(),
        relays: env.relays.clone(),
        log: env.log,
        url: page.url.clone(),
        queries: 0,
        events: 0,
        fetches: 0,
        fetched: Arc::new(AtomicU64::new(0)),
        logged: 0,
        inputs: Vec::new(),
    };
    let job = Job { script: script.digest.clone(), page: Some(page.clone()), now: env.now, deadline: env.deadline };
    let mut session = match Session::open(script.clone(), job, Some(world)).await {
        Ok(session) => session,
        Err(error) => {
            let _ = done.send(Err(error));
            return;
        }
    };
    let spawn = asked.elapsed();

    let answer = match session.next().await {
        Ok((FromWorker::Returned(returned), payload)) => Ok((returned, payload)),
        Ok((FromWorker::Failed(error), _)) => {
            let _ = done.send(Err(error));
            return session.close();
        }
        Ok(_) => Err(confused()),
        Err(error) => Err(error),
    };
    let (returned, payload) = match answer {
        Ok(answer) => answer,
        Err(error) => {
            let _ = done.send(Err(error));
            return;
        }
    };
    let inputs = session.world.as_ref().map_or(&[][..], |world| &world.inputs);
    let (mut ran, pending) = match read(&script, &page, *returned, payload, inputs) {
        Ok(read) => read,
        Err(error) => {
            let _ = done.send(Err(error));
            return;
        }
    };
    ran.timings.spawn = spawn;

    // The fields go now; the image follows, unless it's drawn already.
    if !pending {
        let _ = done.send(Ok(ran));
        return session.close();
    }
    let (drawn, image) = oneshot::channel();
    if let Some(preview) = &mut ran.preview {
        preview.image = Image::Pending(image);
    }
    let version = ran.version.clone();
    let _ = done.send(Ok(ran));
    let draw = !(env.have_image)(&version);
    if session.send(&ToWorker::Draw(draw), &[]).await.is_err() || !draw {
        return session.close();
    }

    session.deadline = session.started + env.deadline * 2 + GRACE;
    let drawing = Instant::now();
    let warn = |error: &str| env.log.warn("link_preview_script_failed", fields! { "url" => page.url.as_str(), "err_msg" => error });
    let bytes = match session.next().await {
        Ok((FromWorker::Drawn(true), bytes)) => {
            session.close();
            let bytes = picture(bytes.to_vec());
            if bytes.is_none() {
                warn(IMAGE_FORMAT);
            }
            bytes
        }
        Ok((FromWorker::Drawn(false), _)) => {
            session.close();
            None
        }
        Ok((FromWorker::Failed(error), _)) => {
            session.close();
            warn(&error);
            None
        }
        Ok(_) => {
            warn(&confused());
            None
        }
        Err(error) => {
            warn(&error);
            None
        }
    };
    env.log.debug("link_preview_drawn", fields! { "url" => page.url.as_str(), "drawn" => bytes.is_some(), "ms" => drawing.elapsed().as_millis() as u64 });
    let _ = drawn.send(bytes);
}

/// What a worker said its handler returned, held to what npanel accepts.
/// Whether an image is to follow.
fn read(script: &Script, page: &Page, returned: sandbox::Returned, payload: Bytes, inputs: &[String]) -> Result<(Ran, bool), String> {
    // A header it wasn't given can't vary what it made.
    let mut vary: Vec<String> = returned.headers.into_iter().filter(|name| HEADERS.contains(&name.as_str())).collect();
    vary.sort();
    vary.dedup();
    let version = version(script, page, &vary, inputs);
    let timings = returned.timings;
    if returned.none {
        return Ok((Ran { preview: None, vary, version, timings }, false));
    }
    let (image, pending) = match returned.image {
        Picture::None => (Image::None, false),
        Picture::Url(url) => (Image::Url(url), false),
        Picture::Bytes => (Image::Bytes(picture(payload.to_vec()).ok_or(IMAGE_FORMAT)?), false),
        Picture::Pending => (Image::None, true),
    };
    let extras = match returned.extras {
        Some(extras) => serde_json::from_str::<Extras>(&extras)
            .map_err(|_| "preview() returned a type, twitter, video, audio, published or jsonLd npanel can't read.".to_string())?,
        None => Extras::default(),
    };
    let preview = Returned { title: returned.title, description: returned.description, body: returned.body, image, extras };
    Ok((Ran { preview: Some(preview), vary, version, timings }, pending))
}

fn confused() -> String {
    "The preview script's process said something npanel didn't expect.".into()
}

/// What a run may ask of the world, and what it has asked.
struct World {
    source: Arc<dyn EventSource>,
    http: reqwest::Client,
    relays: Vec<String>,
    log: Logger,
    url: String,
    queries: usize,
    events: usize,
    fetches: usize,
    /// Bytes fetched in all, counted as they arrive.
    fetched: Arc<AtomicU64>,
    logged: usize,
    /// What its queries and fetches asked for and found, in order.
    inputs: Vec<String>,
}

type Response = (u16, String, Vec<(String, String)>, Vec<u8>);

/// A query or a fetch, done.
enum Answer {
    Queried { call: u64, asked: String, found: Vec<Event> },
    Fetched { call: u64, url: String, result: Result<Response, String> },
}

/// One job on one worker: the gateway's end of the conversation, which
/// answers what the worker asks on the way to what the job is for.
///
/// Dropped rather than closed — after a deadline, a worker gone quiet, or
/// anything else that leaves the conversation in doubt — its process is
/// killed.
struct Session {
    lease: Lease,
    channel: Framed<tokio::net::UnixStream, LengthDelimitedCodec>,
    script: Arc<Script>,
    /// When the job reached the worker.
    started: Instant,
    /// When the worker is killed if it hasn't answered.
    deadline: Instant,
    world: Option<World>,
    calls: FuturesUnordered<BoxFuture<'static, Answer>>,
}

impl Session {
    async fn open(script: Arc<Script>, job: Job, world: Option<World>) -> Result<Session, String> {
        let mut lease = sandbox::pool().lease().await?;
        let stream = lease.take_stream();
        let stream = stream.set_nonblocking(true).and_then(|_| tokio::net::UnixStream::from_std(stream)).map_err(|e| e.to_string())?;
        let now = Instant::now();
        let mut session =
            Session { lease, channel: Framed::new(stream, sandbox::codec()), script, started: now, deadline: now + STARTUP, world, calls: FuturesUnordered::new() };
        if session.lease.process().fresh {
            match session.next().await? {
                (FromWorker::Ready { faces, unconfined }, _) => sandbox::pool().announce(faces, &unconfined),
                _ => return Err(confused()),
            }
            session.lease.process().fresh = false;
        }
        session.started = Instant::now();
        session.deadline = session.started + job.deadline + GRACE;
        session.send(&ToWorker::Job(job), &[]).await?;
        Ok(session)
    }

    /// Done, the worker's job finished: its process goes back for the next.
    fn close(self) {
        let Session { lease, channel, .. } = self;
        let parts = channel.into_parts();
        // Anything still in transit means the two disagree about where the
        // conversation is.
        if !parts.read_buf.is_empty() || !parts.write_buf.is_empty() {
            return;
        }
        if let Ok(stream) = parts.io.into_std() {
            lease.give_back(stream);
        }
    }

    async fn send(&mut self, message: &ToWorker, payload: &[u8]) -> Result<(), String> {
        match tokio::time::timeout_at(self.deadline.into(), self.channel.send(sandbox::encode(message, payload))).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(self.stopped()),
            Err(_) => Err(PAST_DEADLINE.into()),
        }
    }

    /// Why the worker stopped answering.
    fn stopped(&mut self) -> String {
        match self.lease.process().ended() {
            Some(how) => format!("{STOPPED} Its process {how}."),
            None => STOPPED.into(),
        }
    }

    /// The worker's next word that isn't a question for the gateway,
    /// answering those on the way.
    async fn next(&mut self) -> Result<(FromWorker, Bytes), String> {
        loop {
            tokio::select! {
                frame = self.channel.next() => {
                    let Some(Ok(frame)) = frame else { return Err(self.stopped()) };
                    let (message, payload) = sandbox::decode::<FromWorker>(frame).map_err(|_| confused())?;
                    match message {
                        FromWorker::Want => {
                            let source = self.script.source.clone();
                            self.send(&ToWorker::Source(source), &[]).await?;
                        }
                        FromWorker::Query { call, filters } => self.query(call, &filters).await?,
                        FromWorker::Fetch { call, url } => self.fetch(call, url).await?,
                        FromWorker::Log { level, message } => self.log(&level, &message),
                        other => return Ok((other, payload)),
                    }
                }
                Some(answer) = self.calls.next(), if !self.calls.is_empty() => self.answer(answer).await?,
                _ = tokio::time::sleep_until(self.deadline.into()) => return Err(PAST_DEADLINE.into()),
            }
        }
    }

    async fn query(&mut self, call: u64, filters: &str) -> Result<(), String> {
        let Some(world) = &mut self.world else { return self.send(&ToWorker::Queried { call, result: Err(DEPLOYING.into()) }, &[]).await };
        let checked = parse_filters(filters).and_then(|filters| {
            if world.queries >= MAX_QUERIES {
                return Err(format!("A preview may query relays {MAX_QUERIES} times at most."));
            }
            world.queries += 1;
            Ok(filters)
        });
        match checked {
            Ok(filters) => {
                let asked = serde_json::to_string(&filters.iter().map(Filter::to_json).collect::<Vec<_>>()).expect("filters serialize");
                let (source, relays) = (world.source.clone(), world.relays.clone());
                self.calls.push(async move { Answer::Queried { call, asked, found: query(source, filters, relays).await } }.boxed());
                Ok(())
            }
            Err(error) => self.send(&ToWorker::Queried { call, result: Err(error) }, &[]).await,
        }
    }

    async fn fetch(&mut self, call: u64, url: String) -> Result<(), String> {
        let Some(world) = &mut self.world else { return self.send(&ToWorker::Fetched { call, result: Err(DEPLOYING.into()) }, &[]).await };
        let checked = fetch_target(&url).and_then(|target| {
            if world.fetches >= MAX_FETCHES {
                return Err(format!("A preview may fetch {MAX_FETCHES} times at most."));
            }
            world.fetches += 1;
            Ok(target)
        });
        match checked {
            Ok(target) => {
                let (http, fetched) = (world.http.clone(), world.fetched.clone());
                self.calls.push(async move { Answer::Fetched { call, url, result: get(http, target, fetched).await } }.boxed());
                Ok(())
            }
            Err(error) => self.send(&ToWorker::Fetched { call, result: Err(error) }, &[]).await,
        }
    }

    async fn answer(&mut self, answer: Answer) -> Result<(), String> {
        let world = self.world.as_mut().expect("only a run asks");
        match answer {
            Answer::Queried { call, asked, mut found } => {
                found.truncate(MAX_EVENTS - world.events);
                world.events += found.len();
                let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
                world.inputs.push(format!("q {asked} {}", ids.join(",")));
                let events = serde_json::to_string(&found).expect("events serialize");
                self.send(&ToWorker::Queried { call, result: Ok(events) }, &[]).await
            }
            Answer::Fetched { call, url, result: Ok((status, final_url, headers, body)) } => {
                world.inputs.push(format!("f {url} {status}"));
                let fetched = sandbox::Fetched { status, url: final_url, headers };
                self.send(&ToWorker::Fetched { call, result: Ok(fetched) }, &body).await
            }
            Answer::Fetched { call, result: Err(error), .. } => self.send(&ToWorker::Fetched { call, result: Err(error) }, &[]).await,
        }
    }

    fn log(&mut self, level: &str, message: &str) {
        let Some(world) = &mut self.world else { return };
        if world.logged >= MAX_LOG_LINES {
            return;
        }
        world.logged += 1;
        let level = crate::site_preview::clamp(level, 16);
        let message = crate::site_preview::clamp(message, MAX_LOG_LINE);
        world.log.debug("link_preview_script_log", fields! { "url" => world.url.as_str(), "level" => level, "message" => message });
    }
}

/// What names a preview's image: everything it's drawn from.
fn version(script: &Script, page: &Page, vary: &[String], inputs: &[String]) -> String {
    let mut hash = Sha256::new();
    hash.update(format!("{}\0{}\0", script.digest, page.url).as_bytes());
    for name in vary {
        let value = page.headers.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect::<Vec<_>>().join(", ");
        hash.update(format!("{name}={value}\0").as_bytes());
    }
    for input in inputs {
        hash.update(input.as_bytes());
        hash.update(b"\0");
    }
    hex::encode(&hash.finalize()[..16])
}

/// A picture's bytes, if they're of a format and size npanel serves.
fn picture(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let size = imagesize::blob_size(&bytes).ok()?;
    let ok = matches!(
        imagesize::image_type(&bytes).ok()?,
        imagesize::ImageType::Png | imagesize::ImageType::Jpeg | imagesize::ImageType::Gif | imagesize::ImageType::Webp
    );
    (ok && size.width <= canvas::MAX_PICTURE as usize && size.height <= canvas::MAX_PICTURE as usize).then_some(bytes)
}

/// NIP-01 filters, as a script wrote them. Each must name ids, authors or a
/// tag, so it asks about something in particular.
fn parse_filters(json: &str) -> Result<Vec<Filter>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|_| "Filters must be JSON objects.".to_string())?;
    let serde_json::Value::Array(filters) = value else { return Err("query takes an array of filters.".into()) };
    if filters.is_empty() || filters.len() > 10 {
        return Err("query takes one to ten filters.".into());
    }
    filters.iter().map(parse_filter).collect()
}

fn parse_filter(value: &serde_json::Value) -> Result<Filter, String> {
    let serde_json::Value::Object(fields) = value else { return Err("A filter must be an object.".into()) };
    let strings = |key: &str, value: &serde_json::Value, valid: &dyn Fn(&str) -> bool| -> Result<Vec<String>, String> {
        let list = value.as_array().ok_or_else(|| format!("A filter's {key} must be a list."))?;
        if list.len() > MAX_VALUES {
            return Err(format!("A filter's {key} may list {MAX_VALUES} values at most."));
        }
        list.iter()
            .map(|v| v.as_str().filter(|s| valid(s)).map(str::to_string).ok_or_else(|| format!("{v} isn't a valid value for a filter's {key}.")))
            .collect()
    };
    let hex = |s: &str| is_hex64(s) && s == s.to_ascii_lowercase();
    let integer = |key: &str, value: &serde_json::Value| value.as_u64().ok_or_else(|| format!("A filter's {key} must be a whole number."));
    let mut filter = Filter::default();
    for (key, value) in fields {
        match key.as_str() {
            "ids" => filter.ids = strings(key, value, &hex)?,
            "authors" => filter.authors = strings(key, value, &hex)?,
            "kinds" => {
                let list = value.as_array().ok_or("A filter's kinds must be a list.")?;
                if list.len() > MAX_VALUES {
                    return Err(format!("A filter's kinds may list {MAX_VALUES} values at most."));
                }
                filter.kinds = list
                    .iter()
                    .map(|k| k.as_i64().filter(|k| (0..=65_535).contains(k)).ok_or_else(|| "A filter's kinds must be numbers from 0 to 65535.".to_string()))
                    .collect::<Result<_, _>>()?;
            }
            "since" => filter.since = Some(integer(key, value)? as i64),
            "until" => filter.until = Some(integer(key, value)? as i64),
            "limit" => filter.limit = Some(integer(key, value)?.min(MAX_EVENTS as u64)),
            tag if tag.len() == 2 && tag.starts_with('#') && tag.as_bytes()[1].is_ascii_alphabetic() => {
                let values = strings(key, value, &|s: &str| s.len() <= MAX_TAG_VALUE)?;
                filter.tags.insert(tag.as_bytes()[1] as char, values);
            }
            other => return Err(format!("{other} isn't a filter field npanel sends. Use ids, authors, kinds, #<letter>, since, until or limit.")),
        }
    }
    let empty = |list: &Vec<String>| list.is_empty();
    if (filter.ids.is_empty() && filter.authors.is_empty() && filter.tags.is_empty()) || filter.tags.values().any(empty) {
        return Err("A filter must name ids, authors or a tag, so it asks about something in particular.".into());
    }
    Ok(filter)
}

/// What relays have for `filters`: of each filter's matches, the newest it
/// asked for, together and newest first. Configured relays and the page's
/// hints are asked; when they have nothing, the outboxes of the authors the
/// filters name.
async fn query(source: Arc<dyn EventSource>, filters: Vec<Filter>, relays: Vec<String>) -> Vec<Event> {
    let mut events = source.query(filters.clone(), relays).await;
    if events.is_empty() {
        let authors: BTreeSet<&String> = filters.iter().flat_map(|f| &f.authors).collect();
        if !authors.is_empty() && authors.len() <= MAX_OUTBOXES {
            let mut outboxes = Vec::new();
            for author in authors {
                outboxes.extend(outbox(source.as_ref(), author).await);
            }
            let outboxes = dedupe(outboxes);
            if !outboxes.is_empty() {
                events = source.query(filters.clone(), outboxes).await;
            }
        }
    }
    select(&filters, events)
}

fn select(filters: &[Filter], events: Vec<Event>) -> Vec<Event> {
    let newest = |a: &Event, b: &Event| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id));
    let mut out: Vec<Event> = Vec::new();
    for filter in filters {
        let mut matched: Vec<&Event> = events.iter().filter(|e| filter.matches(e)).collect();
        matched.sort_by(|a, b| newest(a, b));
        matched.truncate(filter.limit.map_or(MAX_EVENTS, |l| l as usize));
        out.extend(matched.into_iter().cloned());
    }
    out.sort_by(newest);
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// A URL a script may fetch: `https://`, and not at an address that isn't
/// public. A name is checked by the resolver as it's dialed; an address
/// written out is never resolved, so it's checked here.
fn fetch_target(url: &str) -> Result<url::Url, String> {
    let target = url::Url::parse(url).map_err(|_| format!("{url} isn't a valid URL."))?;
    if target.scheme() != "https" {
        return Err("A preview can only fetch https:// URLs.".into());
    }
    if !public_literal(&target) {
        return Err(format!("{url} isn't on a public address, so a preview can't fetch it."));
    }
    Ok(target)
}

/// Whether a URL's host, if it's an address, is a public one.
fn public_literal(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(_)) => true,
        Some(url::Host::Ipv4(address)) => is_public_address(address.into()),
        Some(url::Host::Ipv6(address)) => is_public_address(address.into()),
        None => false,
    }
}

/// One `https://` fetch: its status, final URL, headers and body. `fetched`
/// counts the run's bytes in all, as they arrive, so fetches at once can't
/// hold more between them than one run may.
async fn get(http: reqwest::Client, url: url::Url, fetched: Arc<AtomicU64>) -> Result<Response, String> {
    let mut response = http.get(url.clone()).send().await.map_err(|e| format!("Fetching {url} failed: {}", e.without_url()))?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let headers = response.headers().iter().filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string()))).collect();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("Fetching {url} failed: {}", e.without_url()))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_FETCH_BYTES {
            return Err(format!("{url} is larger than 8 MB."));
        }
        if fetched.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64 > MAX_FETCHED_BYTES {
            return Err("A preview may fetch 32 MB at most, in all.".into());
        }
    }
    Ok((status, final_url, headers, body))
}

/// The client a script's `fetch` goes through. Every address it connects to
/// is checked to be public, on every hop, and a redirect is followed only to
/// another `https://` URL. No proxy from the environment: one would dial
/// what the resolver never saw.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(crate::net::PublicResolver))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 || attempt.url().scheme() != "https" || !public_literal(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("npanel/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(FETCH_TIMEOUT)
        .timeout(FETCH_TIMEOUT)
        .build()
        .expect("an HTTP client")
}

#[cfg(test)]
mod tests;
