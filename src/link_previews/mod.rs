//! Link previews, and something for search engines to read, on a site's
//! pages — drawn by the site itself.
//!
//! A site with `link_previews` on that publishes a preview script
//! (`script/`) has the HTML pages its script's routes name — a Nostr client's
//! `/<nip19>` and `/<name@domain>` pages, say — answered for crawlers with
//! whatever the script makes of them: a title, a description, an image, and
//! an article at the top of the body. Everyone else, and any page the script
//! has nothing for, gets the page as published. npanel has no idea of its own
//! what a page should say.
//!
//! Serving sites comes first. Scripts run on a runtime and a thread of their
//! own, and serving only ever waits on one briefly: a run that can't start —
//! as many are running as may, or relays have been asked too much this
//! minute — or that takes too long sends the page as published, never an
//! error. A run that outlives the request it started for goes on, and fills
//! the cache for the next one.
//!
//! What a script made is kept in memory, and in `previews.sqlite3` beside the
//! gateway's database, for `link_preview_ttl` seconds. A stale preview is
//! still served, while a fresh one is made behind it.

mod images;
pub mod page;
pub mod preview;
pub mod relays;
pub mod script;
pub mod splice;
mod store;

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

use self::page::Hints;
use self::images::Images;
use self::preview::{absolute, Drawn, Preview};
use self::relays::{Batcher, EventSource};
use self::script::{Env, Image, Page, Returned, Script};
use self::store::Cached;
use crate::db::Db;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::nostr::pool::Pool;
use crate::nostr::service::vet_relay_hints;
use crate::rate_limit::{RateLimit, RateLimiter};
use crate::settings::{Settings, SettingsValues};

/// The crawlers that get a preview, as `link_preview_crawler_pattern` starts.
/// Search engines, and the services that unfurl a link pasted into a chat or
/// a post.
pub const DEFAULT_CRAWLER_PATTERN: &str = "googlebot|bingbot|yandex|baiduspider|duckduckbot|applebot|twitterbot|discordbot|\
facebookexternalhit|facebookcatalog|meta-externalagent|linkedinbot|slackbot|slack-imgproxy|telegrambot|whatsapp|\
embedly|iframely|quora link preview|showyoubot|outbrain|pinterest|vkshare|redditbot|skypeuripreview|mastodon|\
pleroma|akkoma|misskey|rocket\\.chat|cardyb|w3c_validator|rogerbot";

/// The longest User-Agent read. Real ones are a few hundred characters, and
/// a pattern needn't run over the rest of one that isn't.
const MAX_USER_AGENT: usize = 512;

/// How long a request waits for a preview before sending the page as it is.
pub const WAIT: Duration = Duration::from_millis(2500);
/// How long a request for a preview's image waits for one. Longer, since
/// the alternative is no image rather than a page, and it usually follows
/// the page's own run.
pub const IMAGE_WAIT: Duration = Duration::from_secs(8);
/// How long a refresh an admin asked for may take: a run, and its image.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a preview image a site's script drew is, on every site: under this,
/// at the path of the page it's for.
pub const PREVIEW_PATH: &str = "/_npanel/preview";

/// Pages whose scripts' request headers are remembered in memory. The
/// database remembers the rest.
const MAX_VARIES: usize = 10_000;

/// What previews in memory may take up. Past it, the least recently used
/// go; the database still has them.
const MEMORY_BYTES: usize = 32 << 20;

/// How often, in rows written, the database is trimmed to
/// `link_preview_max_entries`.
const EVICT_EVERY: u64 = 64;

pub struct LinkPreviewsConfig {
    /// Where `previews.sqlite3` and drawn images live.
    pub data_dir: PathBuf,
    /// Where events come from. `None` for the relays in the settings; tests
    /// stand in for them.
    pub source: Option<Arc<dyn EventSource>>,
}

/// What the previews runtime has, once it is up.
struct Worker {
    runtime: tokio::runtime::Handle,
    store: Db,
    path: PathBuf,
    source: Arc<dyn EventSource>,
    /// What scripts fetch through.
    http: reqwest::Client,
    images: Arc<Images>,
    /// Scripts that may run at once: one a core, since drawing is work.
    runs: Arc<Semaphore>,
}

/// A page a site's preview script previews.
pub struct ScriptPage {
    pub script: Arc<Script>,
    pub page: Page,
    pub hints: Hints,
}

impl ScriptPage {
    /// What its previews are cached under, before the headers it reads.
    fn base(&self) -> String {
        format!("s:{}:{}", self.script.digest, self.page.url)
    }

    /// What its preview is cached under, given the headers its script reads.
    fn key(&self, vary: &[String]) -> String {
        let mut hash = Sha256::new();
        for name in vary {
            let value = self.page.headers.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect::<Vec<_>>().join(", ");
            hash.update(format!("{name}={value}\0").as_bytes());
        }
        format!("{}|{}", self.base(), &hex::encode(hash.finalize())[..16])
    }

    /// What it's cached under now: by the headers its script was last seen
    /// to read, or, before it has run, by nothing known yet.
    fn current_key(&self, shared: &Shared) -> String {
        match shared.varies.lock().get(&self.base()) {
            Some(vary) => self.key(vary),
            None => format!("{}|?", self.base()),
        }
    }
}

/// What asking for a page's preview came to.
pub enum Answer {
    Preview(Arc<Preview>),
    /// The script had nothing for the page.
    Nothing,
    /// No answer could be had in time.
    Unanswered,
}

impl From<Cached> for Answer {
    fn from(cached: Cached) -> Answer {
        cached.preview.map_or(Answer::Nothing, Answer::Preview)
    }
}

/// Counts since startup, for the dashboard.
#[derive(Default)]
struct Stats {
    hits: AtomicU64,
    lookups: AtomicU64,
    runs: AtomicU64,
    failures: AtomicU64,
    busy: AtomicU64,
    timeouts: AtomicU64,
    over_budget: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatsSnapshot {
    /// Answered from memory.
    pub hits: u64,
    /// Handed to the previews runtime: the database, then the script.
    pub lookups: u64,
    /// Lookups that ran a script.
    pub runs: u64,
    /// Runs whose script threw, or ran out of time.
    pub failures: u64,
    /// Pages sent as published because as many lookups were running as may.
    pub busy: u64,
    /// Pages sent as published because the lookup took too long.
    pub timeouts: u64,
    /// Lookups that didn't run a script because this minute's runs were spent.
    pub over_budget: u64,
}

/// The dashboard's view of link previews.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// Pages with a preview.
    pub found: i64,
    /// Pages a script had nothing for.
    pub empty: i64,
    /// What the cache takes up on disk.
    pub bytes: u64,
    /// Previews held in memory.
    pub in_memory: usize,
    /// Lookups running now.
    pub running: usize,
    pub stats: StatsSnapshot,
}

/// Why something an admin asked of link previews didn't happen.
#[derive(Debug)]
pub enum PreviewError {
    Starting,
    TimedOut,
    /// The script failed, and why.
    Script(String),
    Failed(anyhow::Error),
}

impl PreviewError {
    /// A sentence for the dashboard, for any but `Script`, which says what
    /// the script threw.
    pub fn message(&self) -> &'static str {
        match self {
            PreviewError::Starting => "Link previews are starting. Try again in a moment.",
            PreviewError::TimedOut => "The site's preview script took too long. Try again in a moment.",
            PreviewError::Script(_) => "The site's preview script failed.",
            PreviewError::Failed(_) => "Link previews failed. The server log has the details.",
        }
    }
}

/// The crawler pattern, compiled, and the settings it was read from: `None`
/// for one that didn't compile.
type Crawlers = (Arc<SettingsValues>, Option<Arc<Regex>>);

struct Shared {
    settings: Arc<Settings>,
    log: Logger,
    config: LinkPreviewsConfig,
    worker: RwLock<Option<Arc<Worker>>>,
    memory: Mutex<Memory>,
    /// Lookups under way, by key, with whoever is waiting on each.
    flights: Mutex<HashMap<String, Vec<oneshot::Sender<Option<Cached>>>>>,
    running: AtomicUsize,
    /// Runs that may go to relays, a minute's worth at a time.
    budget: Mutex<RateLimiter>,
    crawlers: RwLock<Option<Crawlers>>,
    stats: Stats,
    writes: AtomicU64,
    /// The request headers each page's script read, by the page.
    varies: Mutex<lru::LruCache<String, Vec<String>>>,
}

pub struct LinkPreviews {
    shared: Arc<Shared>,
    shutdown: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LinkPreviews {
    /// Start the previews runtime. Returns at once; until it is ready, every
    /// page is sent as published.
    pub fn start(settings: Arc<Settings>, log: Logger, config: LinkPreviewsConfig) -> LinkPreviews {
        let shared = Arc::new(Shared {
            settings,
            log,
            config,
            worker: RwLock::new(None),
            memory: Mutex::new(Memory::default()),
            flights: Mutex::new(HashMap::new()),
            running: AtomicUsize::new(0),
            budget: Mutex::new(RateLimiter::new(1)),
            crawlers: RwLock::new(None),
            stats: Stats::default(),
            writes: AtomicU64::new(0),
            varies: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_VARIES).unwrap())),
        });
        let shutdown = CancellationToken::new();

        let (s, stop) = (shared.clone(), shutdown.clone());
        let thread = crate::supervise::supervise("previews", 1, log, move |runtime| {
            let result = runtime.block_on(run(&s, &stop));
            *s.worker.write() = None;
            match result {
                _ if stop.is_cancelled() => Ok(()),
                Ok(()) => Err("stopped".into()),
                Err(error) => Err(error.to_string()),
            }
        });

        LinkPreviews { shared, shutdown, thread: Mutex::new(Some(thread)) }
    }

    pub async fn stop(&self) {
        self.shutdown.cancel();
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }

    fn worker(&self) -> Option<Arc<Worker>> {
        self.shared.worker.read().clone()
    }

    /// Whether a User-Agent is a crawler's, by `link_preview_crawler_pattern`.
    pub fn is_crawler(&self, user_agent: Option<&str>) -> bool {
        let Some(user_agent) = user_agent.filter(|ua| !ua.is_empty()) else { return false };
        let mut end = user_agent.len().min(MAX_USER_AGENT);
        while !user_agent.is_char_boundary(end) {
            end -= 1;
        }
        let user_agent = &user_agent[..end];

        let settings = self.shared.settings.get();
        if let Some((read, pattern)) = &*self.shared.crawlers.read() {
            if Arc::ptr_eq(read, &settings) {
                return pattern.as_ref().is_some_and(|p| p.is_match(user_agent));
            }
        }
        // Validated when it was saved, so this only fails for a pattern that
        // was valid under some other build's limits.
        let pattern = crawler_pattern(&settings.link_preview_crawler_pattern).ok().map(Arc::new);
        let matched = pattern.as_ref().is_some_and(|p| p.is_match(user_agent));
        *self.shared.crawlers.write() = Some((settings, pattern));
        matched
    }

    /// The preview a site's script makes of a page, if it can be had within
    /// `wait`.
    pub async fn preview(&self, page: Arc<ScriptPage>, wait: Duration) -> Answer {
        self.find(page, false, wait).await
    }

    /// Run a page's script again, whatever is cached: for an image that has
    /// been let go since it was drawn.
    pub async fn redraw(&self, page: Arc<ScriptPage>, wait: Duration) -> Answer {
        self.find(page, true, wait).await
    }

    /// The request headers a page's script read, as far as is known.
    pub fn vary(&self, page: &ScriptPage) -> Vec<String> {
        self.shared.varies.lock().get(&page.base()).cloned().unwrap_or_default()
    }

    /// An image a site's script drew, waiting up to `wait` for one still
    /// being drawn.
    pub async fn image(&self, version: &str, wait: Duration) -> Option<Bytes> {
        self.worker()?.images.wait(version, wait).await
    }

    async fn find(&self, page: Arc<ScriptPage>, again: bool, wait: Duration) -> Answer {
        let key = page.current_key(&self.shared);
        let ttl = self.shared.settings.get().link_preview_ttl;

        let remembered = if again { None } else { self.shared.memory.lock().get(&key) };
        if let Some(cached) = remembered {
            self.shared.stats.hits.fetch_add(1, Ordering::Relaxed);
            if !cached.fresh(ttl, crate::util::now()) {
                // Nobody waits on this one: the stale preview goes out now.
                drop(self.begin(&page, again, &key));
            }
            return cached.into();
        }

        let Some(answer) = self.begin(&page, again, &key) else {
            self.shared.stats.busy.fetch_add(1, Ordering::Relaxed);
            return Answer::Unanswered;
        };
        match tokio::time::timeout(wait, answer).await {
            Ok(Ok(Some(cached))) => cached.into(),
            Ok(_) => Answer::Unanswered,
            Err(_) => {
                self.shared.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                Answer::Unanswered
            }
        }
    }

    /// Wait on the lookup for `key`, starting one if none is under way and
    /// one may start. `None` when one may not.
    fn begin(&self, page: &Arc<ScriptPage>, again: bool, key: &str) -> Option<oneshot::Receiver<Option<Cached>>> {
        let (tx, rx) = oneshot::channel();
        let mut flights = self.shared.flights.lock();
        if let Some(waiting) = flights.get_mut(key) {
            waiting.push(tx);
            return Some(rx);
        }

        let worker = self.worker()?;
        let permit = Permit::take(&self.shared)?;
        flights.insert(key.to_string(), vec![tx]);
        drop(flights);

        self.shared.stats.lookups.fetch_add(1, Ordering::Relaxed);
        let flight = Flight { shared: self.shared.clone(), key: key.to_string() };
        worker.runtime.spawn(lookup(self.shared.clone(), worker.clone(), page.clone(), again, flight, permit));
        Some(rx)
    }

    /// Run a page's script again, whatever is cached, wait for its image,
    /// and keep what it made. For an admin, so it neither waits for nor
    /// counts against the limits a crawler is held to, and a script that
    /// fails says why.
    pub async fn refresh(&self, page: Arc<ScriptPage>) -> Result<Option<Arc<Preview>>, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let shared = self.shared.clone();
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            let page = page.clone();
            async move {
                let (cached, key) = match run_script(&shared, &worker, &page).await {
                    Ok(ran) => ran,
                    Err(error) => return Ok(Err(error)),
                };
                keep(&shared, &worker, &key, &cached).await?;
                if let Some(drawn) = cached.preview.as_ref().and_then(|p| p.drawn.as_ref()) {
                    worker.images.wait(&drawn.version, REFRESH_TIMEOUT).await;
                }
                anyhow::Ok(Ok((cached, key)))
            }
        });
        let (cached, key) = match tokio::time::timeout(REFRESH_TIMEOUT, task).await {
            Ok(Ok(Ok(Ok(ran)))) => ran,
            Ok(Ok(Ok(Err(error)))) => return Err(PreviewError::Script(error)),
            Ok(Ok(Err(error))) => return Err(PreviewError::Failed(error)),
            Ok(Err(error)) => return Err(PreviewError::Failed(error.into())),
            Err(_) => return Err(PreviewError::TimedOut),
        };
        self.shared.log.info("link_preview_refreshed", fields! { "key" => key.as_str(), "found" => cached.preview.is_some() });
        Ok(cached.preview)
    }

    /// What is cached, and how lookups have gone since startup.
    pub async fn status(&self) -> Result<Status, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move {
                let counts = worker.store.call(|conn| store::counts(conn)).await?;
                let path = worker.path.clone();
                let bytes = tokio::task::spawn_blocking(move || store::disk_bytes(&path)).await?;
                anyhow::Ok((counts, bytes))
            }
        });
        let ((found, empty), bytes) = task.await.map_err(|e| PreviewError::Failed(e.into()))?.map_err(PreviewError::Failed)?;

        let stats = &self.shared.stats;
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Ok(Status {
            found,
            empty,
            bytes,
            in_memory: self.shared.memory.lock().entries.len(),
            running: self.shared.running.load(Ordering::Relaxed),
            stats: StatsSnapshot {
                hits: read(&stats.hits),
                lookups: read(&stats.lookups),
                runs: read(&stats.runs),
                failures: read(&stats.failures),
                busy: read(&stats.busy),
                timeouts: read(&stats.timeouts),
                over_budget: read(&stats.over_budget),
            },
        })
    }

    /// Forget every preview and image, in memory and on disk. Returns how
    /// many previews there were.
    pub async fn clear(&self) -> Result<usize, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move { worker.store.call(|conn| store::clear(conn)).await }
        });
        let cleared = task.await.map_err(|e| PreviewError::Failed(e.into()))?.map_err(PreviewError::Failed)?;
        self.shared.memory.lock().clear();
        self.shared.varies.lock().clear();
        let images = worker.images.clone();
        tokio::task::spawn_blocking(move || images.clear()).await.map_err(|e| PreviewError::Failed(e.into()))?;
        self.shared.log.info("link_previews_cleared", fields! { "entries" => cleared });
        Ok(cleared)
    }
}

/// A pattern for `link_preview_crawler_pattern`, matched case-insensitively,
/// with its size bounded so a pattern can't cost much to match.
pub fn crawler_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(true).size_limit(1 << 20).build()
}

/// Everything the previews runtime does, until it is told to stop.
async fn run(shared: &Arc<Shared>, stop: &CancellationToken) -> anyhow::Result<()> {
    let path = shared.config.data_dir.join("previews.sqlite3");
    tokio::fs::create_dir_all(&shared.config.data_dir).await?;
    {
        let path = path.clone();
        tokio::task::spawn_blocking(move || store::migrate(&crate::db::open_connection(&path)?)).await??;
    }
    let store = Db::connect(&path)?;

    let source = match &shared.config.source {
        Some(source) => source.clone(),
        None => {
            // Its own pool, so a relay that hangs on a script's query holds
            // up nothing a deploy is waiting for.
            let settings = shared.settings.clone();
            let pool = Pool::new(Arc::new(move |url: &str| settings.get().relays.iter().any(|relay| relay == url)), shared.log);
            Arc::new(Batcher::start(pool, shared.settings.clone())) as Arc<dyn EventSource>
        }
    };

    let images = {
        let dir = shared.config.data_dir.join("preview-images");
        Arc::new(tokio::task::spawn_blocking(move || Images::open(dir)).await??)
    };
    // Read now, so a run seldom waits on them, and an admin learns of a
    // gateway without fonts before a preview comes out without text. Not
    // waited for: a page that draws no text needn't.
    let log = shared.log;
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        match script::canvas::fonts().len() {
            0 => log.warn("link_preview_no_fonts", fields! {}),
            faces => log.info("link_preview_fonts", fields! { "faces" => faces, "ms" => started.elapsed().as_millis() as u64 }),
        }
    });
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let worker = Arc::new(Worker {
        runtime: tokio::runtime::Handle::current(),
        store,
        path,
        source,
        http: script::http_client(),
        images,
        runs: Arc::new(Semaphore::new(cores)),
    });
    *shared.worker.write() = Some(worker);
    shared.log.info("link_previews_started", fields! {});

    stop.cancelled().await;
    Ok(())
}

/// Find a page's preview: in the database, then by running its script.
/// Whoever is waiting is answered as soon as there is anything to answer
/// with.
async fn lookup(shared: Arc<Shared>, worker: Arc<Worker>, page: Arc<ScriptPage>, again: bool, flight: Flight, _permit: Permit) {
    let mut key = flight.key.clone();
    let settings = shared.settings.get();

    // A page whose script's headers aren't remembered in memory may be in
    // the database, and its preview with them.
    if key.ends_with("|?") {
        let base = page.base();
        if let Ok(Some(vary)) = worker.store.call(move |conn| store::get_vary(conn, &base)).await {
            key = page.key(&vary);
            shared.varies.lock().put(page.base(), vary);
        }
    }

    let stored = {
        let k = key.clone();
        match worker.store.call(move |conn| store::get(conn, &k)).await {
            Ok(stored) => stored,
            Err(error) => {
                shared.log.error("link_preview_read_failed", with_err(fields! { "key" => key.as_str() }, error));
                None
            }
        }
    };
    if let Some(cached) = stored.as_ref().filter(|_| !again) {
        shared.memory.lock().put(key.clone(), cached.clone());
        flight.answer(Some(cached.clone()));
        if cached.fresh(settings.link_preview_ttl, crate::util::now()) {
            return;
        }
    }

    let limit = RateLimit { capacity: settings.link_preview_misses_per_minute as f64, interval_ms: 60_000.0 };
    if !shared.budget.lock().take("relays", limit) {
        shared.stats.over_budget.fetch_add(1, Ordering::Relaxed);
        flight.answer(stored);
        return;
    }

    let started = Instant::now();
    let stored_preview = stored.and_then(|stored| stored.preview);
    let (cached, key) = match run_script(&shared, &worker, &page).await {
        // Nothing where there was something more likely means the relays were
        // slow than that it's gone. What was there stays, and is tried again
        // as soon as a page the script had nothing for would be.
        Ok((Cached { preview: None, .. }, key)) if stored_preview.is_some() => (Cached { preview: stored_preview, fetched_at: stale_soon(&settings) }, key),
        Ok(ran) => ran,
        Err(error) => {
            shared.log.warn("link_preview_script_failed", fields! { "url" => page.page.url.as_str(), "err_msg" => error });
            // What it made last time stands, likewise.
            let cached = Cached { preview: stored_preview, fetched_at: stale_soon(&settings) };
            (cached, key)
        }
    };
    shared.log.debug("link_preview_ran", fields! { "key" => key.as_str(), "found" => cached.preview.is_some(), "ms" => started.elapsed().as_millis() as u64 });
    if let Err(error) = keep(&shared, &worker, &key, &cached).await {
        shared.log.error("link_preview_write_failed", with_err(fields! { "key" => key.as_str() }, error));
    }
    flight.answer(Some(cached));
}

/// A `fetched_at` that is stale as soon as a page a script had nothing for
/// would be.
fn stale_soon(settings: &SettingsValues) -> i64 {
    crate::util::now() - settings.link_preview_ttl + store::EMPTY_TTL.min(settings.link_preview_ttl)
}

/// Run a page's script, and remember which headers it read. What it made,
/// and the key it's kept under; or why it failed.
async fn run_script(shared: &Shared, worker: &Worker, page: &ScriptPage) -> Result<(Cached, String), String> {
    shared.stats.runs.fetch_add(1, Ordering::Relaxed);
    let permit = worker.runs.clone().acquire_owned().await.map_err(|e| e.to_string())?;
    let images = worker.images.clone();
    let env = Env {
        runtime: worker.runtime.clone(),
        source: worker.source.clone(),
        http: worker.http.clone(),
        relays: vet_relay_hints(&page.hints.relays).await,
        log: shared.log,
        have_image: Arc::new(move |version| images.has(version) || images.is_drawing(version)),
        now: crate::util::now() * 1000,
        deadline: script::DEADLINE,
    };
    let ran = match script::run(page.script.clone(), page.page.clone(), env, permit).await {
        Ok(ran) => ran,
        Err(error) => {
            shared.stats.failures.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
    };
    let t = ran.timings;
    let ms = |d: Duration| (d.as_secs_f64() * 10_000.0).round() / 10.0;
    shared.log.debug(
        "link_preview_script_ran",
        fields! {
            "url" => page.page.url.as_str(),
            "spawn_ms" => ms(t.spawn),
            "setup_ms" => ms(t.setup),
            "prelude_ms" => ms(t.prelude),
            "module_ms" => ms(t.module),
            "handler_ms" => ms(t.handler),
            "heap" => t.heap,
        },
    );

    let key = page.key(&ran.vary);
    shared.varies.lock().put(page.base(), ran.vary.clone());
    let (base, vary) = (page.base(), ran.vary);
    if let Err(error) = worker.store.call(move |conn| store::put_vary(conn, &base, &vary)).await {
        shared.log.error("link_preview_write_failed", with_err(fields! { "key" => key.as_str() }, error));
    }

    let now = crate::util::now();
    let cached = match ran.preview {
        Some(returned) => Cached { preview: Some(Arc::new(preview(worker, page, returned, ran.version).await)), fetched_at: now },
        None => Cached { preview: None, fetched_at: now },
    };
    Ok((cached, key))
}

/// The preview a script's return makes, its image kept to be served, if it
/// drew one.
async fn preview(worker: &Worker, page: &ScriptPage, returned: Returned, version: String) -> Preview {
    let url = &page.page.url;
    let served = || {
        let parsed = url::Url::parse(url).ok()?;
        Some(format!("{}{PREVIEW_PATH}{}?v={version}", parsed.origin().ascii_serialization(), parsed.path()))
    };
    let (image, drawn) = match returned.image {
        Image::None => (None, None),
        Image::Url(named) => (absolute(url, &named), None),
        Image::Bytes(bytes) => {
            let size = imagesize::blob_size(&bytes).ok();
            let mime = imagesize::image_type(&bytes).ok().map(|kind| match kind {
                imagesize::ImageType::Png => "image/png",
                imagesize::ImageType::Gif => "image/gif",
                imagesize::ImageType::Webp => "image/webp",
                _ => "image/jpeg",
            });
            worker.images.put(&version, bytes).await;
            let drawn = Drawn { version: version.clone(), mime: mime.map(String::from), width: size.map(|s| s.width as u32), height: size.map(|s| s.height as u32) };
            (served(), Some(drawn))
        }
        Image::Pending(image) => {
            worker.images.drawing(version.clone(), image);
            (served(), Some(Drawn { version: version.clone(), mime: None, width: None, height: None }))
        }
    };
    let mut preview = Preview::new(url, returned.title, returned.description, returned.body, returned.extras);
    preview.drawn = drawn.filter(|_| image.is_some());
    preview.image = image;
    preview
}

/// Remember what a run made, in memory and on disk.
async fn keep(shared: &Arc<Shared>, worker: &Worker, key: &str, cached: &Cached) -> anyhow::Result<()> {
    shared.memory.lock().put(key.to_string(), cached.clone());

    let evict = shared.writes.fetch_add(1, Ordering::Relaxed).is_multiple_of(EVICT_EVERY);
    let max = shared.settings.get().link_preview_max_entries;
    let (key, cached) = (key.to_string(), cached.clone());
    worker
        .store
        .call(move |conn| {
            store::put(conn, &key, &cached)?;
            if evict {
                store::evict(conn, max)?;
            }
            Ok(())
        })
        .await
}

/// One of the lookups that may run at once, given back when dropped.
struct Permit(Arc<Shared>);

impl Permit {
    /// A permit, if fewer than `link_preview_concurrency` are out. Read every
    /// time, so a change applies to the next lookup.
    fn take(shared: &Arc<Shared>) -> Option<Permit> {
        let limit = usize::try_from(shared.settings.get().link_preview_concurrency).unwrap_or(1);
        let mut running = shared.running.load(Ordering::Relaxed);
        loop {
            if running >= limit {
                return None;
            }
            match shared.running.compare_exchange_weak(running, running + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Some(Permit(shared.clone())),
                Err(now) => running = now,
            }
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A lookup under way. Dropped — however the lookup ends, a panic included —
/// it stops being one, and whoever still waits on it is let go.
struct Flight {
    shared: Arc<Shared>,
    key: String,
}

impl Flight {
    /// Answer everyone waiting so far. Anyone who joins after waits for the
    /// next answer.
    fn answer(&self, value: Option<Cached>) {
        let waiting = self.shared.flights.lock().get_mut(&self.key).map(std::mem::take).unwrap_or_default();
        for waiter in waiting {
            let _ = waiter.send(value.clone());
        }
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.shared.flights.lock().remove(&self.key);
    }
}

/// Previews in memory, least recently used first out, bounded by what they
/// take up rather than by how many there are: an article is a hundred notes.
struct Memory {
    entries: lru::LruCache<String, Cached>,
    bytes: usize,
}

impl Default for Memory {
    fn default() -> Self {
        Memory { entries: lru::LruCache::unbounded(), bytes: 0 }
    }
}

impl Memory {
    fn get(&mut self, key: &str) -> Option<Cached> {
        self.entries.get(key).cloned()
    }

    fn put(&mut self, key: String, cached: Cached) {
        self.bytes += key.len() + cached.weight();
        if let Some((old_key, old)) = self.entries.push(key, cached) {
            self.bytes -= old_key.len() + old.weight();
        }
        while self.bytes > MEMORY_BYTES {
            let Some((key, old)) = self.entries.pop_lru() else { break };
            self.bytes -= key.len() + old.weight();
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_crawlers() {
        let pattern = crawler_pattern(DEFAULT_CRAWLER_PATTERN).unwrap();
        for crawler in [
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)",
            "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)",
            "Twitterbot/1.0",
            "Slackbot-LinkExpanding 1.0 (+https://api.slack.com/robots)",
            "TelegramBot (like TwitterBot)",
            "WhatsApp/2.23.20.0",
            "Mastodon/4.2.0 (http.rb/5.1.1; +https://mastodon.social/)",
            "Mozilla/5.0 (compatible; Bluesky Cardyb/1.1; +mailto:support@bsky.app)",
        ] {
            assert!(pattern.is_match(crawler), "{crawler}");
        }
        for browser in [
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
            "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0",
        ] {
            assert!(!pattern.is_match(browser), "{browser}");
        }
    }

    #[test]
    fn memory_is_bounded_by_size() {
        let mut memory = Memory::default();
        let empty = || Cached { preview: None, fetched_at: 0 };
        memory.put("a".into(), empty());
        memory.put("a".into(), empty());
        assert_eq!(memory.bytes, 1 + 64);
        for i in 0..(MEMORY_BYTES / 64) {
            memory.put(format!("{i}"), empty());
        }
        assert!(memory.bytes <= MEMORY_BYTES);
        assert!(memory.get("a").is_none());
        memory.clear();
        assert_eq!(memory.bytes, 0);
    }
}
