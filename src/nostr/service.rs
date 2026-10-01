//! Relay access for the sync worker.
//!
//! Every query reads the relay list from settings, so an admin editing it in
//! the dashboard takes effect without a restart. The live subscription is the
//! exception: the sync worker restarts that itself when the relays change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::pool::{Filter, Pool};
use super::Event;
use crate::fields;
use crate::identifiers::parse_relay_url;
use crate::log::Logger;
use crate::manifest::{relay_list_tags, server_tags};
use crate::nsite_address::{NsiteAddress, NAMED_SITE_KIND};
use crate::settings::Settings;

/// NIP-65 relay list.
const RELAY_LIST_KIND: i64 = 10002;
/// BUD-03 Blossom server list.
const SERVER_LIST_KIND: i64 = 10063;

/// How long a cached relay or server list is trusted before refetching.
const LIST_STALE_TIME: Duration = Duration::from_secs(60 * 60);

const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Most relays taken from one author's kind 10002 list. Every entry is a
/// connection the deploy thread opens and holds; a handful covers any honest
/// outbox.
pub const MAX_AUTHOR_RELAYS: usize = 5;

/// Most Blossom servers taken from a manifest and its author's list together.
/// A blob is asked of each in turn until one has it, so every extra name is
/// another request per missing file, at an address of the author's choosing.
pub const MAX_AUTHOR_SERVERS: usize = 8;

/// What the sync worker asks of Nostr. A trait so tests can stand in for the
/// relays.
pub trait NostrSource: Send + Sync {
    /// Relays configured in the dashboard.
    fn relays(&self) -> Vec<String>;

    /// The newest manifest event for a site, if any relay has one.
    fn fetch_manifest<'a>(&'a self, address: &'a NsiteAddress, cancel: &'a CancellationToken) -> BoxFuture<'a, Option<Event>>;

    /// Write relays for many authors in one round trip.
    fn authors_relays<'a>(&'a self, pubkeys: &'a [String], cancel: &'a CancellationToken) -> BoxFuture<'a, HashMap<String, Vec<String>>>;

    /// Blossom servers to try for a site's blobs, best first.
    fn blob_sources<'a>(&'a self, manifest: &'a Event, fallbacks: &'a [String], cancel: &'a CancellationToken) -> BoxFuture<'a, Vec<String>>;

    /// Stream new events matching `filters` until `cancel` fires.
    fn subscribe(&self, filters: Vec<Filter>, relays: &[String], cancel: CancellationToken) -> mpsc::Receiver<Event>;

    /// Forget cached relay and server lists, forcing a refetch.
    fn clear_caches(&self);
}

struct CachedList {
    value: Vec<String>,
    fetched_at: Instant,
}

pub struct NostrService {
    pool: Pool,
    settings: Arc<Settings>,
    log: Logger,
    relay_lists: Mutex<HashMap<String, CachedList>>,
    server_lists: Mutex<HashMap<String, CachedList>>,
}

impl NostrService {
    pub fn new(settings: Arc<Settings>, log: Logger) -> Self {
        let configured = settings.clone();
        Self {
            pool: Pool::new(Arc::new(move |url: &str| configured.get().relays.iter().any(|relay| relay == url)), log),
            settings,
            log,
            relay_lists: Mutex::new(HashMap::new()),
            server_lists: Mutex::new(HashMap::new()),
        }
    }

    /// Collect events matching `filters` from the configured relays and
    /// `extra`, giving up after the query timeout.
    async fn query(&self, filters: Vec<Filter>, extra: &[String], cancel: &CancellationToken) -> Vec<Event> {
        let relays = dedupe(extra.iter().cloned().chain(self.relays()));
        if relays.is_empty() {
            return Vec::new();
        }
        self.pool.query(filters, &relays, QUERY_TIMEOUT, cancel).await
    }

    async fn get_authors_relays(&self, pubkeys: &[String], cancel: &CancellationToken) -> HashMap<String, Vec<String>> {
        let mut lists = HashMap::new();
        let mut misses = Vec::new();

        {
            let cache = self.relay_lists.lock();
            for pubkey in dedupe(pubkeys.iter().cloned()) {
                match cache.get(&pubkey) {
                    Some(cached) if cached.fetched_at.elapsed() < LIST_STALE_TIME => {
                        lists.insert(pubkey, cached.value.clone());
                    }
                    _ => misses.push(pubkey),
                }
            }
        }

        if misses.is_empty() {
            return lists;
        }

        // Kind 10002 is replaceable, so relays hold at most one event per
        // author and a combined filter needs no `limit`.
        let events = self
            .query(vec![Filter { kinds: vec![RELAY_LIST_KIND], authors: misses.clone(), ..Default::default() }], &[], cancel)
            .await;
        let mut by_author: HashMap<&str, Vec<&Event>> = HashMap::new();
        for event in &events {
            by_author.entry(&event.pubkey).or_default().push(event);
        }

        for pubkey in misses {
            let newest = latest(by_author.get(pubkey.as_str()).map(Vec::as_slice).unwrap_or(&[]));
            let value = match newest {
                Some(event) => vet_relay_hints(&relay_list_tags(event).1).await,
                None => Vec::new(),
            };
            self.log.debug("relay_list_fetched", fields! { "pubkey" => pubkey.as_str(), "relays" => value.len() });
            self.relay_lists.lock().insert(pubkey.clone(), CachedList { value: value.clone(), fetched_at: Instant::now() });
            lists.insert(pubkey, value);
        }

        lists
    }

    async fn get_author_servers(&self, pubkey: &str, cancel: &CancellationToken) -> Vec<String> {
        if let Some(cached) = self.server_lists.lock().get(pubkey).filter(|c| c.fetched_at.elapsed() < LIST_STALE_TIME) {
            return cached.value.clone();
        }

        let events = self
            .query(
                vec![Filter { kinds: vec![SERVER_LIST_KIND], authors: vec![pubkey.to_string()], limit: Some(1), ..Default::default() }],
                &[],
                cancel,
            )
            .await;
        let refs: Vec<&Event> = events.iter().collect();
        let value = latest(&refs).map(server_tags).unwrap_or_default();

        self.log.debug("server_list_fetched", fields! { "pubkey" => pubkey, "servers" => value.len() });
        self.server_lists.lock().insert(pubkey.to_string(), CachedList { value: value.clone(), fetched_at: Instant::now() });
        value
    }
}

impl NostrSource for NostrService {
    fn relays(&self) -> Vec<String> {
        self.settings.get().relays.clone()
    }

    fn fetch_manifest<'a>(&'a self, address: &'a NsiteAddress, cancel: &'a CancellationToken) -> BoxFuture<'a, Option<Event>> {
        Box::pin(async move {
            // A manifest is most likely on its author's write relays.
            let author_relays =
                self.get_authors_relays(std::slice::from_ref(&address.pubkey), cancel).await.remove(&address.pubkey).unwrap_or_default();
            let events = self.query(vec![manifest_filter(address, None)], &author_relays, cancel).await;
            let refs: Vec<&Event> = events.iter().collect();
            latest(&refs).cloned()
        })
    }

    fn authors_relays<'a>(&'a self, pubkeys: &'a [String], cancel: &'a CancellationToken) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(self.get_authors_relays(pubkeys, cancel))
    }

    fn blob_sources<'a>(&'a self, manifest: &'a Event, fallbacks: &'a [String], cancel: &'a CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(async move {
            let author_servers = self.get_author_servers(&manifest.pubkey, cancel).await;
            let named = server_tags(manifest).into_iter().chain(author_servers).filter_map(|server| normalize_server(&server));
            // The admin's fallbacks are not the author's to crowd out.
            let named = dedupe(named).into_iter().take(MAX_AUTHOR_SERVERS);
            dedupe(named.chain(fallbacks.iter().filter_map(|server| normalize_server(server))))
        })
    }

    fn subscribe(&self, filters: Vec<Filter>, relays: &[String], cancel: CancellationToken) -> mpsc::Receiver<Event> {
        self.pool.subscribe(filters, relays, cancel)
    }

    fn clear_caches(&self) {
        self.relay_lists.lock().clear();
        self.server_lists.lock().clear();
    }
}

/// Filter matching a site's manifest events.
pub fn manifest_filter(address: &NsiteAddress, since: Option<i64>) -> Filter {
    Filter {
        kinds: vec![address.kind],
        authors: vec![address.pubkey.clone()],
        d: if address.kind == NAMED_SITE_KIND { vec![address.identifier.clone()] } else { Vec::new() },
        since,
        limit: None,
    }
}

/// The URL a blob would live at on a Blossom server.
pub fn blob_url(server: &str, sha256: &str) -> String {
    match url::Url::parse(server).and_then(|base| base.join(sha256)) {
        Ok(url) => url.to_string(),
        Err(_) => format!("{server}{sha256}"),
    }
}

/// The relays from an author's list npanel will connect to: well-formed
/// `wss://` URLs on public addresses, at most [`MAX_AUTHOR_RELAYS`].
///
/// The same rule blob downloads follow, for the same reason: the list is
/// written by whoever publishes a site. Checking here spends the few slots on
/// relays that can pass; the pool checks again at connect, on the addresses
/// it dials, which is what a rebinding name can't get past. Only the first
/// few are resolved at all, since each check is a DNS lookup and the list can
/// be as long as its author likes.
pub async fn vet_relay_hints(urls: &[String]) -> Vec<String> {
    let candidates: Vec<String> = dedupe(urls.iter().filter_map(|url| parse_relay_url(url).ok()))
        .into_iter()
        .take(MAX_AUTHOR_RELAYS * 4)
        .collect();

    let verdicts = futures_util::future::join_all(candidates.iter().map(|candidate| async move {
        match url::Url::parse(candidate) {
            Ok(url) => crate::net::is_public_url(&url).await.unwrap_or(false),
            Err(_) => false,
        }
    }))
    .await;

    candidates.into_iter().zip(verdicts).filter(|(_, ok)| *ok).map(|(url, _)| url).take(MAX_AUTHOR_RELAYS).collect()
}

fn normalize_server(server: &str) -> Option<String> {
    let mut url = url::Url::parse(server).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Some(url.to_string())
}

/// The newest of some events; the first seen wins a tie.
fn latest<'a>(events: &[&'a Event]) -> Option<&'a Event> {
    events.iter().copied().fold(None, |newest: Option<&Event>, event| match newest {
        Some(newest) if event.created_at <= newest.created_at => Some(newest),
        _ => Some(event),
    })
}

/// Order-preserving deduplication.
pub fn dedupe<T: Clone + Eq + std::hash::Hash>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    values.into_iter().filter(|value| seen.insert(value.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_urls() {
        assert_eq!(blob_url("https://b.example.com/", &"a".repeat(64)), format!("https://b.example.com/{}", "a".repeat(64)));
        assert_eq!(blob_url("https://b.example.com/prefix/", "x"), "https://b.example.com/prefix/x");
        assert_eq!(normalize_server("https://b.example.com/prefix").as_deref(), Some("https://b.example.com/prefix/"));
        assert_eq!(normalize_server("wss://b.example.com"), None);
    }

    #[tokio::test]
    async fn vets_relay_hints() {
        let hints = vec![
            "wss://127.0.0.1".to_string(),
            "ws://1.1.1.1".to_string(),
            "wss://1.1.1.1/".to_string(),
            "wss://1.1.1.1".to_string(),
            "wss://[::1]".to_string(),
        ];
        assert_eq!(vet_relay_hints(&hints).await, ["wss://1.1.1.1"]);
    }
}
