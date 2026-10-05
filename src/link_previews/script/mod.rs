//! Link previews a site draws for itself, with a script.
//!
//! A site publishes `/.npanel/preview.js`, which exports
//! `default { preview(request, ctx) }`: given the crawler's request and a
//! read-only Nostr relay, it returns the page's title, description and
//! article, and its image — a URL, or bytes it drew on an `OffscreenCanvas`,
//! or a promise of them — or `null`, which leaves the page as published. Modules it
//! imports sit beside it in `/.npanel/`; it draws text in the fonts installed
//! on the system.
//!
//! Each run gets a QuickJS runtime of its own, on a thread of its own, with a
//! memory limit, a deadline, and the web a preview needs (`prelude.js`) and
//! nothing else: no file, and no network but `nostr` and an `https://`-only
//! `fetch` through the public-addresses-only resolver, both metered.
//!
//! The image is the slow part, so a run hands back the page's fields as soon
//! as the handler returns them, and goes on drawing behind them. What names
//! the image — its version — is a hash of everything the handler was given
//! until then: the script, the URL, the request headers it read, and what its
//! queries and fetches asked for and found. So it is known before the image
//! is, and changes when anything the image is drawn from changes.

pub mod canvas;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use parking_lot::Mutex;
use rquickjs::function::Opt;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::prelude::Promised;
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Exception, Function, Module, Object, Promise, TypedArray, Value};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, OwnedSemaphorePermit};

use self::canvas::{Font, Line, State, Style, Surfaces, Text};
use super::preview::Extras;
use super::relays::{outbox, EventSource};
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::fields;
use crate::hash::BlobHash;
use crate::log::Logger;
use crate::nostr::pool::Filter;
use crate::nostr::service::dedupe;
use crate::nostr::{is_hex64, Event};

/// Where a site keeps its script and the modules it imports.
pub const DIR: &str = "/.npanel/";
/// The module npanel runs, under [`DIR`].
pub const ENTRY: &str = "preview.js";

const PRELUDE: &str = include_str!("prelude.js");

const MAX_SCRIPT_BYTES: u64 = 4 << 20;
const MAX_SCRIPTS_BYTES: u64 = 8 << 20;

/// What one run's JavaScript may hold, and how deep it may call.
const MEMORY: usize = 64 << 20;
const STACK: usize = 1 << 20;
/// How long a handler has to return. Its image has as long again.
pub const DEADLINE: Duration = Duration::from_secs(10);

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

/// A build's script: its modules, by their path under [`DIR`].
pub struct Script {
    /// Names what the script is: every module. Two builds with the same
    /// script share previews.
    pub digest: String,
    modules: HashMap<String, Arc<str>>,
}

/// Something wrong with a site's `/.npanel/`, for its deploy log.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

/// What a build's `/.npanel/` comes to, and what was wrong with it.
pub struct Loaded {
    pub script: Option<Script>,
    pub problems: Vec<Problem>,
}

/// Read a build's files under [`DIR`]. A build with no `preview.js` has no
/// script, whatever else it has there. Blocking.
pub fn load(files: &[(String, BlobHash)], blobs: &BlobStore) -> Loaded {
    let mut problems = Vec::new();
    let mut problem = |path: &str, message: &str| problems.push(Problem { path: path.to_string(), message: message.to_string() });
    let mut modules = HashMap::new();
    let mut total = 0;
    let mut digest = Sha256::new();

    for (path, sha256) in files {
        let Some(name) = path.strip_prefix(DIR) else { continue };
        let lower = name.to_ascii_lowercase();
        let size = blobs.size(*sha256);
        if lower.ends_with(".js") || lower.ends_with(".mjs") {
            let Some(size) = size.filter(|size| *size <= MAX_SCRIPT_BYTES) else {
                problem(path, "This module is larger than 4 MB, or missing, so it isn't loaded.");
                continue;
            };
            total += size;
            if total > MAX_SCRIPTS_BYTES {
                problem(path, "A site's modules may come to 8 MB at most, so this one isn't loaded.");
                continue;
            }
            match String::from_utf8(blobs.read_bytes(*sha256, MAX_SCRIPT_BYTES)) {
                Ok(source) => {
                    modules.insert(name.to_string(), Arc::from(source));
                    digest.update(format!("{name}\0{sha256}\0").as_bytes());
                }
                Err(_) => problem(path, "This module isn't UTF-8 text, so it isn't loaded."),
            }
        } else if lower.ends_with(".ttf") || lower.ends_with(".otf") {
            problem(path, "Previews draw text in the fonts installed on the gateway, not a site's own, so this font is ignored. Ask the gateway's admin for any it lacks.");
        } else {
            problem(path, "Only JavaScript modules belong in /.npanel/, so this file is ignored.");
        }
    }

    if !modules.contains_key(ENTRY) {
        if !modules.is_empty() {
            problem(&format!("{DIR}{ENTRY}"), "There are modules in /.npanel/ but no preview.js, so none of them run.");
        }
        return Loaded { script: None, problems };
    }
    let script = Script { digest: hex::encode(&digest.finalize()[..16]), modules };
    Loaded { script: Some(script), problems }
}

/// The files under [`DIR`] in a build.
pub fn build_files(conn: &Connection, build_id: i64) -> rusqlite::Result<Vec<(String, BlobHash)>> {
    // Every path under the directory sorts between it and the next string
    // after its `/`, so this is a range of the primary key.
    conn.prepare_cached("SELECT path, sha256 FROM host_build_files WHERE build_id = ? AND path >= ? AND path < ? ORDER BY path")?
        .query_map(rusqlite::params![build_id, DIR, "/.npanel0"], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// Builds' scripts, read once each. A build's files never change, so neither
/// does its script.
pub struct Scripts {
    db: Db,
    blobs: BlobStore,
    cache: Mutex<lru::LruCache<i64, Option<Arc<Script>>>>,
}

/// Builds whose scripts are kept.
const MAX_CACHED_BUILDS: usize = 64;

impl Scripts {
    pub fn new(db: Db, blobs: BlobStore) -> Scripts {
        Scripts { db, blobs, cache: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_BUILDS).unwrap())) }
    }

    /// A build's script, or `None` for a build without one.
    pub async fn for_build(&self, build_id: i64) -> anyhow::Result<Option<Arc<Script>>> {
        if let Some(cached) = self.cache.lock().get(&build_id) {
            return Ok(cached.clone());
        }
        let blobs = self.blobs.clone();
        let script = self
            .db
            .call(move |conn| {
                let files = build_files(conn, build_id)?;
                Ok(if files.is_empty() { None } else { load(&files, &blobs).script.map(Arc::new) })
            })
            .await?;
        self.cache.lock().put(build_id, script.clone());
        Ok(script)
    }
}

/// The request a handler is given.
#[derive(Clone, Debug)]
pub struct Page {
    /// The page's URL: its origin and path, without the query a crawler may
    /// have added.
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
}

/// What a run needs from the gateway.
pub struct Env {
    /// The previews runtime, where relay queries and fetches happen.
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
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    /// From being asked for to its thread starting.
    pub spawn: Duration,
    /// Making a QuickJS runtime and context.
    pub setup: Duration,
    /// Running the prelude: the web the script runs in.
    pub prelude: Duration,
    /// Parsing the site's modules and running their top level.
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

/// Run a site's handler for one page, holding `permit` — one of the runs
/// that may go at once — until it's done drawing.
pub async fn run(script: Arc<Script>, page: Page, env: Env, permit: OwnedSemaphorePermit) -> Result<Ran, String> {
    let (done, ran) = oneshot::channel();
    spawn(script, Some((page, env)), done, Some(permit), Instant::now())?;
    ran.await.unwrap_or_else(|_| Err("The preview script stopped without an answer.".into()))
}

/// Load a site's script as a run would and check it exports a handler,
/// without calling it, for its deploy log.
pub async fn check(script: Arc<Script>) -> Result<(), String> {
    let (done, ran) = oneshot::channel();
    spawn(script, None, done, None, Instant::now())?;
    ran.await.unwrap_or_else(|_| Err("The preview script stopped without an answer.".into())).map(|_| ())
}

/// A thread for one run, with a runtime of its own to drive it. The thread
/// is a plain one, not the blocking pool's, because a runtime can't be
/// started on a thread that's already in one.
fn spawn(
    script: Arc<Script>,
    job: Option<(Page, Env)>,
    done: oneshot::Sender<Result<Ran, String>>,
    permit: Option<OwnedSemaphorePermit>,
    asked: Instant,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("npanel-preview".into())
        .spawn(move || {
            let _permit = permit;
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
            runtime.block_on(execute(script, job, done, asked));
        })
        .map(|_| ())
        .map_err(|e| format!("No thread for the preview script: {e}"))
}

/// What a run has spent, and what it was given.
#[derive(Default)]
struct Spent {
    queries: Cell<usize>,
    events: Cell<usize>,
    fetches: Cell<usize>,
    fetched: Cell<u64>,
    logged: Cell<usize>,
    /// What its queries and fetches asked for and found, in order.
    inputs: RefCell<Vec<String>>,
}

async fn execute(script: Arc<Script>, job: Option<(Page, Env)>, done: oneshot::Sender<Result<Ran, String>>, asked: Instant) {
    let started = Instant::now();
    let mut timings = Timings { spawn: started - asked, ..Timings::default() };
    let deadline = job.as_ref().map_or(DEADLINE, |(_, env)| env.deadline);
    let limit = Arc::new(AtomicU64::new(deadline.as_millis() as u64));
    let runtime = match AsyncRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = done.send(Err(error.to_string()));
            return;
        }
    };
    runtime.set_memory_limit(MEMORY).await;
    runtime.set_max_stack_size(STACK).await;
    {
        let limit = limit.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || started.elapsed().as_millis() as u64 > limit.load(Ordering::Relaxed)))).await;
    }
    runtime.set_loader(Modules(script.clone()), Modules(script.clone())).await;
    let context = match AsyncContext::full(&runtime).await {
        Ok(context) => context,
        Err(error) => {
            let _ = done.send(Err(error.to_string()));
            return;
        }
    };

    context
        .async_with(async move |ctx| {
            let spent = Rc::new(Spent::default());
            let (page, env) = match job {
                Some((page, env)) => (Some(page), Some(env)),
                None => (None, None),
            };
            let env = env.map(Rc::new);
            let extend = |to: Duration| limit.store(to.as_millis() as u64, Ordering::Relaxed);
            let overran = || started.elapsed().as_millis() as u64 >= limit.load(Ordering::Relaxed);
            let fail = |ctx: &Ctx<'_>, error: rquickjs::Error| {
                if overran() {
                    "The preview script ran past its deadline.".to_string()
                } else {
                    describe(ctx, error)
                }
            };

            timings.setup = started.elapsed();
            let start = async {
                let at = Instant::now();
                let natives = natives(&ctx, env.clone(), page.as_ref().map(|p| p.url.clone()).unwrap_or_default(), spent.clone())?;
                let prelude: Function = ctx.eval(PRELUDE)?;
                let seed = page.as_ref().map_or(0, |page| u32::from_le_bytes(Sha256::digest(page.url.as_bytes())[..4].try_into().unwrap()));
                let now = env.as_ref().map_or(0, |env| env.now);
                let hooks: Object = prelude.call((natives, seed, now as f64))?;
                timings.prelude = at.elapsed();
                let at = Instant::now();
                let module: Object = Module::import(&ctx, ENTRY)?.into_future().await?;
                let handler: Value = module.get("default")?;
                timings.module = at.elapsed();
                rquickjs::Result::Ok((hooks, handler))
            };
            let (hooks, handler) = match start.await {
                Ok(started) => started,
                Err(error) => {
                    let _ = done.send(Err(fail(&ctx, error)));
                    return;
                }
            };
            let (Some(page), Some(env)) = (page, env) else {
                let exported = handler.as_object().and_then(|h| h.get::<_, Value>("preview").ok()).is_some_and(|f| f.is_function());
                let _ = done.send(if exported {
                    Ok(Ran { preview: None, vary: Vec::new(), version: String::new(), timings })
                } else {
                    Err("/.npanel/preview.js must export default { preview(request, ctx) }.".into())
                });
                return;
            };

            let called = Instant::now();
            let call = async {
                let call: Function = hooks.get("call")?;
                let headers = ctx.json_parse(serde_json::to_string(&page.headers).expect("headers serialize"))?;
                let promise: Promise = call.call((handler, page.url.as_str(), page.method.as_str(), headers, deadline.as_millis() as f64))?;
                let out: Object = promise.into_future().await?;
                rquickjs::Result::Ok(out)
            };
            let out = match tokio::time::timeout(deadline, call).await {
                Ok(Ok(out)) => out,
                Ok(Err(error)) => {
                    let _ = done.send(Err(fail(&ctx, error)));
                    return;
                }
                Err(_) => {
                    let _ = done.send(Err("The preview script ran past its deadline.".into()));
                    return;
                }
            };
            timings.handler = called.elapsed();
            timings.heap = heap(&ctx);

            let read = async {
                let mut vary: Vec<String> = out.get("headers")?;
                vary.sort();
                vary.dedup();
                let version = version(&script, &page, &vary, &spent.inputs.borrow());
                if out.get::<_, bool>("none")? {
                    return rquickjs::Result::Ok((Ran { preview: None, vary, version, timings }, None));
                }
                let mut pending = None;
                let image = if let Some(url) = out.get::<_, Option<String>>("url")? {
                    Image::Url(url)
                } else if let Some(bytes) = out.get::<_, Option<TypedArray<u8>>>("bytes")? {
                    match picture(bytes_of(&bytes)) {
                        Some(bytes) => Image::Bytes(bytes),
                        None => return Err(Exception::throw_type(&ctx, "The image must be a PNG, JPEG, GIF or WebP, 4096 pixels a side at most.")),
                    }
                } else if let Some(promise) = out.get::<_, Option<Promise>>("pending")? {
                    pending = Some(promise);
                    Image::None
                } else {
                    Image::None
                };
                let extras: String = out.get("extras")?;
                let Ok(extras) = serde_json::from_str::<Extras>(&extras) else {
                    return Err(Exception::throw_type(&ctx, "preview() returned a type, twitter, video, audio, published or jsonLd npanel can't read."));
                };
                let preview = Returned { title: out.get("title")?, description: out.get("description")?, body: out.get("body")?, image, extras };
                Ok((Ran { preview: Some(preview), vary, version, timings }, pending))
            };
            let (mut ran, pending) = match read.await {
                Ok(read) => read,
                Err(error) => {
                    let _ = done.send(Err(fail(&ctx, error)));
                    return;
                }
            };

            // The fields go now; the image follows, unless it's drawn already.
            let Some(pending) = pending else {
                let _ = done.send(Ok(ran));
                return;
            };
            let (drawn, image) = oneshot::channel();
            if let Some(preview) = &mut ran.preview {
                preview.image = Image::Pending(image);
            }
            let version = ran.version.clone();
            let _ = done.send(Ok(ran));
            if (env.have_image)(&version) {
                return;
            }
            extend(deadline * 2);
            let drawing = Instant::now();
            let bytes = match tokio::time::timeout((deadline * 2).saturating_sub(started.elapsed()), pending.into_future::<Option<TypedArray<u8>>>()).await {
                Ok(Ok(Some(bytes))) => {
                    let bytes = picture(bytes_of(&bytes));
                    if bytes.is_none() {
                        env.log.warn("link_preview_script_failed", fields! { "url" => page.url.as_str(), "err_msg" => "The image must be a PNG, JPEG, GIF or WebP, 4096 pixels a side at most." });
                    }
                    bytes
                }
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    env.log.warn("link_preview_script_failed", fields! { "url" => page.url.as_str(), "err_msg" => fail(&ctx, error) });
                    None
                }
                Err(_) => {
                    env.log.warn("link_preview_script_failed", fields! { "url" => page.url.as_str(), "err_msg" => "The preview's image took too long to draw." });
                    None
                }
            };
            env.log.debug("link_preview_drawn", fields! { "url" => page.url.as_str(), "drawn" => bytes.is_some(), "ms" => drawing.elapsed().as_millis() as u64, "heap" => heap(&ctx) });
            let _ = drawn.send(bytes);
        })
        .await;
}

/// What a run's JavaScript heap holds now, in bytes.
fn heap(ctx: &Ctx<'_>) -> u64 {
    // SAFETY: `ctx` holds its runtime's lock, and the runtime lives as long
    // as it does.
    let usage = unsafe {
        let mut usage = std::mem::MaybeUninit::uninit();
        rquickjs::qjs::JS_ComputeMemoryUsage(rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()), usage.as_mut_ptr());
        usage.assume_init()
    };
    usage.memory_used_size.max(0) as u64
}

/// What a script threw, for a log or a deploy log.
fn describe(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    let thrown = ctx.catch();
    if let Some(exception) = thrown.as_exception() {
        let message = match exception.get::<_, String>("name") {
            Ok(name) if !name.is_empty() => format!("{name}: {}", exception.message().unwrap_or_default()),
            _ => exception.message().unwrap_or_default(),
        };
        return match exception.stack().filter(|stack| !stack.trim().is_empty()) {
            Some(stack) => format!("{message}\n{}", stack.trim_end()),
            None => message,
        };
    }
    match thrown.as_string().and_then(|s| s.to_string().ok()) {
        Some(text) => text,
        None => "The preview script threw something that isn't an Error.".into(),
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

fn bytes_of(array: &TypedArray<'_, u8>) -> Vec<u8> {
    // SAFETY: the bytes are copied out at once, while nothing else runs.
    unsafe { array.as_bytes() }.map(<[u8]>::to_vec).unwrap_or_default()
}

/// The modules under [`DIR`], and nothing else, importable by relative path.
struct Modules(Arc<Script>);

impl Resolver for Modules {
    fn resolve<'js>(&mut self, _: &Ctx<'js>, base: &str, name: &str, _: Option<ImportAttributes<'js>>) -> rquickjs::Result<String> {
        if base.is_empty() && name == ENTRY {
            return Ok(ENTRY.into());
        }
        if !name.starts_with("./") && !name.starts_with("../") {
            return Err(rquickjs::Error::new_resolving_message(base, name, "only modules in /.npanel/ can be imported, by a relative path; bundle packages into them"));
        }
        let mut segments: Vec<&str> = base.split('/').collect();
        segments.pop();
        for segment in name.split('/') {
            match segment {
                "." | "" => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(rquickjs::Error::new_resolving_message(base, name, "that's outside /.npanel/"));
                    }
                }
                other => segments.push(other),
            }
        }
        let resolved = segments.join("/");
        if self.0.modules.contains_key(&resolved) {
            Ok(resolved)
        } else {
            Err(rquickjs::Error::new_resolving_message(base, name, "there's no such module in /.npanel/"))
        }
    }
}

impl Loader for Modules {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, _: Option<ImportAttributes<'js>>) -> rquickjs::Result<Module<'js, Declared>> {
        let source = self.0.modules.get(name).ok_or_else(|| rquickjs::Error::new_loading(name))?;
        Module::declare(ctx.clone(), name, source.as_bytes())
    }
}

fn thrown(ctx: &Ctx<'_>, message: impl std::fmt::Display) -> rquickjs::Error {
    Exception::throw_type(ctx, &message.to_string())
}

/// What `prelude.js` is built on.
fn natives<'js>(ctx: &Ctx<'js>, env: Option<Rc<Env>>, url: String, spent: Rc<Spent>) -> rquickjs::Result<Object<'js>> {
    let natives = Object::new(ctx.clone())?;
    let surfaces = Rc::new(RefCell::new(Surfaces::default()));
    natives.set("utf8Encode", Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| TypedArray::<u8>::new(ctx, text.into_bytes())))?;
    natives.set("utf8Decode", Function::new(ctx.clone(), |bytes: TypedArray<'js, u8>| String::from_utf8_lossy(&bytes_of(&bytes)).into_owned()))?;
    natives.set(
        "btoa",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
            let bytes = text.chars().map(|c| u8::try_from(u32::from(c)).map_err(|_| thrown(&ctx, "btoa only encodes Latin-1 text."))).collect::<rquickjs::Result<Vec<u8>>>()?;
            rquickjs::Result::Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }),
    )?;
    natives.set(
        "atob",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
            let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(compact.trim_end_matches('='))
                .map_err(|_| thrown(&ctx, "atob was given something that isn't base64."))?;
            rquickjs::Result::Ok(bytes.into_iter().map(char::from).collect::<String>())
        }),
    )?;
    natives.set(
        "urlParse",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, input: String, base: Opt<String>| {
            let parsed = match base.0 {
                Some(base) => url::Url::parse(&base).and_then(|base| base.join(&input)),
                None => url::Url::parse(&input),
            }
            .map_err(|_| thrown(&ctx, format!("{input} isn't a valid URL.")))?;
            let parts = Object::new(ctx.clone())?;
            let host = parsed.host_str().unwrap_or_default();
            let port = parsed.port().map(|p| p.to_string()).unwrap_or_default();
            parts.set("href", parsed.as_str())?;
            parts.set("origin", parsed.origin().ascii_serialization())?;
            parts.set("protocol", format!("{}:", parsed.scheme()))?;
            parts.set("username", parsed.username())?;
            parts.set("password", parsed.password().unwrap_or_default())?;
            parts.set("host", if port.is_empty() { host.to_string() } else { format!("{host}:{port}") })?;
            parts.set("hostname", host)?;
            parts.set("port", port)?;
            parts.set("pathname", parsed.path())?;
            parts.set("search", parsed.query().map(|q| format!("?{q}")).unwrap_or_default())?;
            parts.set("hash", parsed.fragment().map(|f| format!("#{f}")).unwrap_or_default())?;
            rquickjs::Result::Ok(parts)
        }),
    )?;
    {
        let (env, spent, url) = (env.clone(), spent.clone(), url.clone());
        natives.set(
            "log",
            Function::new(ctx.clone(), move |level: String, message: String| {
                let Some(env) = &env else { return };
                if spent.logged.get() >= MAX_LOG_LINES {
                    return;
                }
                spent.logged.set(spent.logged.get() + 1);
                let message = crate::site_preview::clamp(&message, MAX_LOG_LINE);
                env.log.debug("link_preview_script_log", fields! { "url" => url.as_str(), "level" => level, "message" => message });
            }),
        )?;
    }
    natives.set("isColor", Function::new(ctx.clone(), |text: String| text.trim().parse::<svgtypes::Color>().is_ok()))?;

    // Canvases and bitmaps.
    let surface = |ctx: &Ctx<'js>, result: Result<(), String>| result.map_err(|e| thrown(ctx, e));
    {
        let s = surfaces.clone();
        natives.set(
            "canvasNew",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, width: u32, height: u32| s.borrow_mut().create(width, height).map(|id| id as u32).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set("surfaceFree", Function::new(ctx.clone(), move |id: u32| s.borrow_mut().free(id as usize)))?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "surfaceCopy",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32| s.borrow_mut().copy(id as usize).map(|id| id as u32).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "surfaceSize",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32| s.borrow().size(id as usize).map(|(w, h)| vec![w, h]).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "fill",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, path: Vec<f64>, rule: String, style: String, state: String| {
                let (style, state) = (parse::<Style>(&ctx, &style)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().fill(id as usize, &path, &rule, &style, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "stroke",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, path: Vec<f64>, style: String, line: String, state: String| {
                let (style, line, state) = (parse::<Style>(&ctx, &style)?, parse::<Line>(&ctx, &line)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().stroke(id as usize, &path, &style, &line, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "drawImage",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, source: u32, rects: Vec<f64>, matrix: Vec<f64>, state: String| {
                let state = parse::<State>(&ctx, &state)?;
                let (Ok(rects), Ok(matrix)) = (<[f64; 8]>::try_from(rects), <[f64; 6]>::try_from(matrix)) else {
                    return Err(thrown(&ctx, "drawImage was given the wrong number of coordinates."));
                };
                let rects = rects.map(|v| v as f32);
                let source_rect = [rects[0], rects[1], rects[2], rects[3]];
                let target = [rects[4], rects[5], rects[6], rects[7]];
                surface(&ctx, s.borrow_mut().draw_image(id as usize, source as usize, source_rect, target, matrix.map(|v| v as f32), &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "getImageData",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, x: f64, y: f64, width: f64, height: f64| {
                let data = s.borrow().get_image_data(id as usize, x as i32, y as i32, width as u32, height as u32).map_err(|e| thrown(&ctx, e))?;
                TypedArray::<u8>::new(ctx, data)
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "putImageData",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, data: TypedArray<'js, u8>, x: f64, y: f64, width: f64, height: f64| {
                surface(&ctx, s.borrow_mut().put_image_data(id as usize, &bytes_of(&data), x as i32, y as i32, width as u32, height as u32))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "drawText",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, text: String, style: String, state: String| {
                let (text, style, state) = (parse::<Text>(&ctx, &text)?, parse::<Style>(&ctx, &style)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().draw_text(id as usize, &text, &style, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "measureText",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String, font: String| {
                let font = parse::<Font>(&ctx, &font)?;
                let m = s.borrow().measure_text(&text, &font).map_err(|e| thrown(&ctx, e))?;
                rquickjs::Result::Ok(vec![m.width, m.left, m.right, m.ascent, m.descent])
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "decode",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, bytes: TypedArray<'js, u8>| {
                s.borrow_mut().decode(bytes_of(&bytes)).map(|id| id as u32).map_err(|e| thrown(&ctx, e))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "encode",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, mime: String, quality: f64| {
                let (bytes, mime) = s.borrow().encode(id as usize, &mime, Some(quality)).map_err(|e| thrown(&ctx, e))?;
                let out = Object::new(ctx.clone())?;
                out.set("bytes", TypedArray::<u8>::new(ctx.clone(), bytes)?)?;
                out.set("type", mime)?;
                rquickjs::Result::Ok(out)
            }),
        )?;
    }

    // Time, relays and the network.
    natives.set(
        "sleep",
        Function::new(ctx.clone(), |ms: f64| {
            Promised(async move {
                tokio::time::sleep(Duration::from_millis(ms.clamp(0.0, 60_000.0) as u64)).await;
            })
        }),
    )?;
    {
        let (env, spent) = (env.clone(), spent.clone());
        natives.set(
            "query",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, filters: String| {
                let Some(env) = env.clone() else { return Err(thrown(&ctx, "Nothing is fetched while a site deploys.")) };
                let filters = parse_filters(&filters).map_err(|e| thrown(&ctx, e))?;
                if spent.queries.get() >= MAX_QUERIES {
                    return Err(thrown(&ctx, format!("A preview may query relays {MAX_QUERIES} times at most.")));
                }
                spent.queries.set(spent.queries.get() + 1);
                let spent = spent.clone();
                let asked = serde_json::to_string(&filters.iter().map(Filter::to_json).collect::<Vec<_>>()).expect("filters serialize");
                let task = env.runtime.spawn(query(env.source.clone(), filters, env.relays.clone()));
                rquickjs::Result::Ok(Promised(async move {
                    let mut found = task.await.unwrap_or_default();
                    found.truncate(MAX_EVENTS - spent.events.get());
                    spent.events.set(spent.events.get() + found.len());
                    let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
                    spent.inputs.borrow_mut().push(format!("q {asked} {}", ids.join(",")));
                    rquickjs::Result::Ok(serde_json::to_string(&found).expect("events serialize"))
                }))
            }),
        )?;
    }
    {
        let (env, spent) = (env, spent);
        natives.set(
            "fetch",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, url: String| {
                let Some(env) = env.clone() else { return Err(thrown(&ctx, "Nothing is fetched while a site deploys.")) };
                let target = url::Url::parse(&url).map_err(|_| thrown(&ctx, format!("{url} isn't a valid URL.")))?;
                if target.scheme() != "https" {
                    return Err(thrown(&ctx, "A preview can only fetch https:// URLs."));
                }
                if spent.fetches.get() >= MAX_FETCHES {
                    return Err(thrown(&ctx, format!("A preview may fetch {MAX_FETCHES} times at most.")));
                }
                spent.fetches.set(spent.fetches.get() + 1);
                let spent = spent.clone();
                let task = env.runtime.spawn(get(env.http.clone(), target));
                let ctx2 = ctx.clone();
                rquickjs::Result::Ok(Promised(async move {
                    let fetched = task.await.map_err(|e| e.to_string()).and_then(|r| r);
                    let (status, final_url, headers, body) = fetched.map_err(|e| thrown(&ctx2, e))?;
                    spent.fetched.set(spent.fetched.get() + body.len() as u64);
                    if spent.fetched.get() > MAX_FETCHED_BYTES {
                        return Err(thrown(&ctx2, "A preview may fetch 32 MB at most, in all."));
                    }
                    spent.inputs.borrow_mut().push(format!("f {url} {status}"));
                    let out = Object::new(ctx2.clone())?;
                    out.set("status", status)?;
                    out.set("url", final_url)?;
                    out.set("headers", ctx2.json_parse(serde_json::to_string(&headers).expect("headers serialize"))?)?;
                    out.set("body", TypedArray::<u8>::new(ctx2.clone(), body)?)?;
                    rquickjs::Result::Ok(out)
                }))
            }),
        )?;
    }
    Ok(natives)
}

fn parse<T: serde::de::DeserializeOwned>(ctx: &Ctx<'_>, json: &str) -> rquickjs::Result<T> {
    serde_json::from_str(json).map_err(|e| thrown(ctx, format!("npanel's canvas was given something it can't read: {e}")))
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

/// One `https://` fetch: its status, final URL, headers and body.
async fn get(http: reqwest::Client, url: url::Url) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>), String> {
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
    }
    Ok((status, final_url, headers, body))
}

/// The client a script's `fetch` goes through. Every address it connects to
/// is checked to be public, on every hop, and a redirect is followed only to
/// another `https://` URL.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(crate::net::PublicResolver))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 || attempt.url().scheme() != "https" {
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
