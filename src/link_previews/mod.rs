//! Link previews, and something for search engines to read, on the pages of
//! a site that names Nostr events in its paths.
//!
//! A site with `link_previews` on has its `/<nip19>` and `/<name@domain>`
//! pages — the way Nostr web clients route a profile, a note or an article —
//! answered for crawlers with the event's own title, description and image,
//! and the event itself as an article at the top of the body. Everyone else
//! gets the page as published.
//!
//! Serving sites comes first. Relay lookups happen on a runtime and a thread
//! of their own, and serving only ever waits on one briefly: a lookup that
//! can't start — the runtime is busy with as many as it may run, or relays
//! have been asked too much this minute — or that takes too long sends the
//! page as published, never an error. A lookup that outlives the request it
//! started for goes on, and fills the cache for the next one.
//!
//! What was found is kept in memory, and in `previews.sqlite3` beside the
//! gateway's database, for `link_preview_ttl` seconds. A stale card is still
//! served, while a fresh one is fetched behind it.

pub mod card;
pub mod entity;
pub mod fetch;
mod render;
pub mod splice;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use self::card::Card;
use self::entity::Entity;
use self::fetch::{Batcher, EventSource, Names};
use self::store::Cached;
use crate::db::Db;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::nostr::pool::Pool;
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

/// How long a request waits for a lookup before sending the page as it is.
const WAIT: Duration = Duration::from_millis(2500);
/// How long a refresh an admin asked for may take.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// What cards in memory may take up. Past it, the least recently used go;
/// the database still has them.
const MEMORY_BYTES: usize = 32 << 20;

/// How often, in rows written, the database is trimmed to
/// `link_preview_max_entries`.
const EVICT_EVERY: u64 = 64;

pub struct LinkPreviewsConfig {
    /// Where `previews.sqlite3` lives.
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
    names: Names,
}

/// Counts since startup, for the dashboard.
#[derive(Default)]
struct Stats {
    hits: AtomicU64,
    lookups: AtomicU64,
    fetches: AtomicU64,
    busy: AtomicU64,
    timeouts: AtomicU64,
    over_budget: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatsSnapshot {
    /// Answered from memory.
    pub hits: u64,
    /// Handed to the previews runtime: the database, then relays.
    pub lookups: u64,
    /// Lookups that went to relays.
    pub fetches: u64,
    /// Pages sent as published because as many lookups were running as may.
    pub busy: u64,
    /// Pages sent as published because the lookup took too long.
    pub timeouts: u64,
    /// Lookups that didn't go to relays because this minute's were spent.
    pub over_budget: u64,
}

/// The dashboard's view of link previews.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    /// Entities with a card.
    pub found: i64,
    /// Entities a lookup found nothing for.
    pub not_found: i64,
    /// What the cache takes up on disk.
    pub bytes: u64,
    /// Cards held in memory.
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
    Failed(anyhow::Error),
}

impl PreviewError {
    /// A sentence for the dashboard, for any but `Failed`.
    pub fn message(&self) -> &'static str {
        match self {
            PreviewError::Starting => "Link previews are starting. Try again in a moment.",
            PreviewError::TimedOut => "The relays took too long to answer. Try again in a moment.",
            PreviewError::Failed(_) => "Link previews failed. The server log has the details.",
        }
    }
}

/// What a refresh found.
#[derive(Clone, Debug)]
pub struct Refreshed {
    pub key: String,
    pub entity: String,
    pub card: Option<Arc<Card>>,
}

/// The crawler pattern, compiled, and the settings it was read from: `None`
/// for one that didn't compile.
type Crawlers = (Arc<SettingsValues>, Option<Arc<Regex>>);

struct Shared {
    /// The gateway's database, for the NIP-05 names its own hosts answer for.
    db: Db,
    settings: Arc<Settings>,
    log: Logger,
    config: LinkPreviewsConfig,
    worker: RwLock<Option<Arc<Worker>>>,
    memory: Mutex<Memory>,
    /// Lookups under way, by key, with whoever is waiting on each.
    flights: Mutex<HashMap<String, Vec<oneshot::Sender<Option<Cached>>>>>,
    running: AtomicUsize,
    /// Lookups that may go to relays, a minute's worth at a time.
    budget: Mutex<RateLimiter>,
    crawlers: RwLock<Option<Crawlers>>,
    stats: Stats,
    writes: AtomicU64,
}

pub struct LinkPreviews {
    shared: Arc<Shared>,
    shutdown: CancellationToken,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl LinkPreviews {
    /// Start the previews runtime. Returns at once; until it is ready, every
    /// page is sent as published.
    pub fn start(db: Db, settings: Arc<Settings>, log: Logger, config: LinkPreviewsConfig) -> LinkPreviews {
        let shared = Arc::new(Shared {
            db,
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

    /// The card for a page a crawler asked for, if one can be had in time.
    /// `None` sends the page as published.
    pub async fn card(&self, entity: &Entity) -> Option<Arc<Card>> {
        let key = entity.key();
        let ttl = self.shared.settings.get().link_preview_ttl;

        let remembered = self.shared.memory.lock().get(&key);
        if let Some(cached) = remembered {
            self.shared.stats.hits.fetch_add(1, Ordering::Relaxed);
            if !cached.fresh(ttl, crate::util::now()) {
                // Nobody waits on this one: the stale card goes out now.
                drop(self.begin(entity, &key));
            }
            return cached.card;
        }

        let Some(answer) = self.begin(entity, &key) else {
            self.shared.stats.busy.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match tokio::time::timeout(WAIT, answer).await {
            Ok(Ok(cached)) => cached.and_then(|cached| cached.card),
            Ok(Err(_)) => None,
            Err(_) => {
                self.shared.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Wait on the lookup for `key`, starting one if none is under way and
    /// one may start. `None` when one may not.
    fn begin(&self, entity: &Entity, key: &str) -> Option<oneshot::Receiver<Option<Cached>>> {
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
        worker.runtime.spawn(lookup(self.shared.clone(), worker.clone(), entity.clone(), flight, permit));
        Some(rx)
    }

    /// Fetch an entity's card again, whatever is cached, and keep what is
    /// found. For an admin, so it neither waits for nor counts against the
    /// limits a crawler is held to.
    pub async fn refresh(&self, entity: Entity) -> Result<Refreshed, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let shared = self.shared.clone();
        let key = entity.key();

        let task = worker.runtime.spawn({
            let worker = worker.clone();
            let key = key.clone();
            let entity = entity.clone();
            async move {
                let card = fetch::fetch(worker.source.as_ref(), &worker.names, &entity).await.map(Arc::new);
                let cached = Cached { card, fetched_at: crate::util::now() };
                keep(&shared, &worker, &key, &cached).await?;
                anyhow::Ok(cached)
            }
        });
        let cached = match tokio::time::timeout(REFRESH_TIMEOUT, task).await {
            Ok(Ok(Ok(cached))) => cached,
            Ok(Ok(Err(error))) => return Err(PreviewError::Failed(error)),
            Ok(Err(error)) => return Err(PreviewError::Failed(error.into())),
            Err(_) => return Err(PreviewError::TimedOut),
        };

        self.shared.log.info("link_preview_refreshed", fields! { "key" => key.as_str(), "found" => cached.card.is_some() });
        Ok(Refreshed { key, entity: entity.label(), card: cached.card })
    }

    /// What is cached, and how lookups have gone since startup.
    pub async fn status(&self) -> Result<Status, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move {
                let (found, not_found) = worker.store.call(|conn| store::counts(conn)).await?;
                let path = worker.path.clone();
                let bytes = tokio::task::spawn_blocking(move || store::disk_bytes(&path)).await?;
                anyhow::Ok((found, not_found, bytes))
            }
        });
        let (found, not_found, bytes) = task.await.map_err(|e| PreviewError::Failed(e.into()))?.map_err(PreviewError::Failed)?;

        let stats = &self.shared.stats;
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Ok(Status {
            found,
            not_found,
            bytes,
            in_memory: self.shared.memory.lock().entries.len(),
            running: self.shared.running.load(Ordering::Relaxed),
            stats: StatsSnapshot {
                hits: read(&stats.hits),
                lookups: read(&stats.lookups),
                fetches: read(&stats.fetches),
                busy: read(&stats.busy),
                timeouts: read(&stats.timeouts),
                over_budget: read(&stats.over_budget),
            },
        })
    }

    /// Forget every card, in memory and on disk. Returns how many there were.
    pub async fn clear(&self) -> Result<usize, PreviewError> {
        let worker = self.worker().ok_or(PreviewError::Starting)?;
        let task = worker.runtime.spawn({
            let worker = worker.clone();
            async move { worker.store.call(|conn| store::clear(conn)).await }
        });
        let cleared = task.await.map_err(|e| PreviewError::Failed(e.into()))?.map_err(PreviewError::Failed)?;
        self.shared.memory.lock().clear();
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
            // Its own pool, so a relay that hangs on a lookup holds up nothing
            // a deploy is waiting for.
            let settings = shared.settings.clone();
            let pool = Pool::new(Arc::new(move |url: &str| settings.get().relays.iter().any(|relay| relay == url)), shared.log);
            Arc::new(Batcher::start(pool, shared.settings.clone())) as Arc<dyn EventSource>
        }
    };

    let worker = Arc::new(Worker { runtime: tokio::runtime::Handle::current(), store, path, source, names: Names::new(shared.db.clone()) });
    *shared.worker.write() = Some(worker);
    shared.log.info("link_previews_started", fields! {});

    stop.cancelled().await;
    Ok(())
}

/// Find a card: in the database, then on relays. Whoever is waiting is
/// answered as soon as there is anything to answer with.
async fn lookup(shared: Arc<Shared>, worker: Arc<Worker>, entity: Entity, flight: Flight, _permit: Permit) {
    let key = flight.key.clone();
    let settings = shared.settings.get();

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
    if let Some(cached) = &stored {
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

    shared.stats.fetches.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let card = fetch::fetch(worker.source.as_ref(), &worker.names, &entity).await.map(Arc::new);
    let now = crate::util::now();
    shared.log.debug("link_preview_fetched", fields! { "key" => key.as_str(), "found" => card.is_some(), "ms" => started.elapsed().as_millis() as u64 });

    let cached = match (card, stored) {
        (Some(card), _) => Cached { card: Some(card), fetched_at: now },
        // Finding nothing where a card was found before more likely means the
        // relays were slow than that the event is gone. The card stays, and
        // is tried again as soon as a page that found nothing would be.
        (None, Some(Cached { card: Some(card), .. })) => {
            Cached { card: Some(card), fetched_at: now - settings.link_preview_ttl + store::NOT_FOUND_TTL.min(settings.link_preview_ttl) }
        }
        (None, _) => Cached { card: None, fetched_at: now },
    };
    if let Err(error) = keep(&shared, &worker, &key, &cached).await {
        shared.log.error("link_preview_write_failed", with_err(fields! { "key" => key.as_str() }, error));
    }
    flight.answer(Some(cached));
}

/// Remember what a lookup found, in memory and on disk.
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

/// Cards in memory, least recently used first out, bounded by what they take
/// up rather than by how many there are: an article is a hundred notes.
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
        let empty = || Cached { card: None, fetched_at: 0 };
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
