//! Sites' preview scripts' queries, batched: queries that arrive within
//! [`BATCH_WINDOW`] of each other share one subscription to each relay.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{join_all, BoxFuture};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::manifest::relay_list_tags;
use crate::nostr::pool::{Filter, Pool};
use crate::nostr::service::{dedupe, vet_relay_hints};
use crate::nostr::Event;
use crate::settings::Settings;

/// How long queries wait for each other before going out as one.
const BATCH_WINDOW: Duration = Duration::from_millis(50);
/// The most queries one subscription carries.
const MAX_BATCH: usize = 200;
/// The most relays a batch adds to the configured ones, from the hints its
/// queries carry.
const MAX_HINTED_RELAYS: usize = 16;
/// How long one subscription waits on relays.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// The most filters one subscription carries. Relays cap the filters in a
/// subscription, and a crawl brings hundreds.
const MAX_FILTERS: usize = 16;

const RELAY_LIST_KIND: i64 = 10002;

/// Where events come from. A trait so tests can stand in for the relays.
pub trait EventSource: Send + Sync {
    /// Every event matching any of `filters`, from the configured relays and
    /// `relays`: of a replaceable or addressable event, only the newest.
    fn query(&self, filters: Vec<Filter>, relays: Vec<String>) -> BoxFuture<'_, Vec<Event>>;
}

struct Ask {
    filters: Vec<Filter>,
    relays: Vec<String>,
    reply: oneshot::Sender<Vec<Event>>,
}

/// A crawl asks for hundreds of pages at once, and a relay sees a handful of
/// subscriptions rather than one apiece — which is what keeps npanel welcome
/// at the relays its deploys also depend on.
pub struct Batcher {
    asks: mpsc::Sender<Ask>,
}

impl Batcher {
    /// Start batching, on the current runtime.
    pub fn start(pool: Pool, settings: Arc<Settings>) -> Batcher {
        let (asks, mut queue) = mpsc::channel::<Ask>(4096);
        tokio::spawn(async move {
            while let Some(first) = queue.recv().await {
                let mut batch = vec![first];
                let deadline = Instant::now() + BATCH_WINDOW;
                while batch.len() < MAX_BATCH {
                    match tokio::time::timeout_at(deadline, queue.recv()).await {
                        Ok(Some(ask)) => batch.push(ask),
                        _ => break,
                    }
                }
                tokio::spawn(query(pool.clone(), settings.get().relays.clone(), batch));
            }
        });
        Batcher { asks }
    }
}

impl EventSource for Batcher {
    fn query(&self, filters: Vec<Filter>, relays: Vec<String>) -> BoxFuture<'_, Vec<Event>> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            if self.asks.send(Ask { filters, relays, reply }).await.is_err() {
                return Vec::new();
            }
            answer.await.unwrap_or_default()
        })
    }
}

/// One subscription for a whole batch, answered query by query.
async fn query(pool: Pool, configured: Vec<String>, batch: Vec<Ask>) {
    let hinted = dedupe(batch.iter().flat_map(|ask| ask.relays.iter().cloned())).into_iter().take(MAX_HINTED_RELAYS);
    let relays = dedupe(configured.into_iter().chain(hinted));
    let filters = merge(batch.iter().flat_map(|ask| ask.filters.iter().cloned()));

    let events: Vec<Event> = if relays.is_empty() {
        Vec::new()
    } else {
        let cancel = CancellationToken::new();
        join_all(filters.chunks(MAX_FILTERS).map(|filters| pool.query(filters.to_vec(), &relays, QUERY_TIMEOUT, &cancel)))
            .await
            .into_iter()
            .flatten()
            .collect()
    };
    for ask in batch {
        let mut ids = HashSet::new();
        let found = events.iter().filter(|event| ask.filters.iter().any(|f| f.matches(event)) && ids.insert(&event.id)).cloned().collect();
        let _ = ask.reply.send(found);
    }
}

/// The batch's filters, each once, with those asking for authors' newest
/// replaceable or addressable events of the same kinds made one: a page of
/// profiles asks every relay one filter, not a filter a profile. The relay
/// pool keeps only the newest of each address however many a relay sends, so
/// the `limit` they lose on the way says nothing a reply doesn't.
fn merge(filters: impl Iterator<Item = Filter>) -> Vec<Filter> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut by_kinds: BTreeMap<(Vec<i64>, Vec<String>), BTreeSet<String>> = BTreeMap::new();
    for filter in filters {
        let newest = !filter.authors.is_empty()
            && filter.ids.is_empty()
            && filter.tags.keys().all(|letter| *letter == 'd')
            && filter.since.is_none()
            && filter.until.is_none()
            && !filter.kinds.is_empty()
            && filter.kinds.iter().all(|kind| addressed(*kind));
        if newest {
            let d = filter.tags.get(&'d').cloned().unwrap_or_default();
            by_kinds.entry((filter.kinds.clone(), d)).or_default().extend(filter.authors);
        } else if seen.insert(filter.to_json().to_string()) {
            out.push(filter);
        }
    }
    for ((kinds, d), authors) in by_kinds {
        let tags = if d.is_empty() { BTreeMap::new() } else { BTreeMap::from([('d', d)]) };
        out.push(Filter { kinds, authors: authors.into_iter().collect(), tags, ..Default::default() });
    }
    out
}

/// Whether events of a kind have one current version per address: a
/// replaceable kind's per author, an addressable one's per author and `d`.
fn addressed(kind: i64) -> bool {
    kind == 0 || kind == 3 || (10_000..20_000).contains(&kind) || (30_000..40_000).contains(&kind)
}

/// The relays an author writes to, as their kind 10002 says.
pub async fn outbox(source: &dyn EventSource, pubkey: &str) -> Vec<String> {
    let filter = Filter { kinds: vec![RELAY_LIST_KIND], authors: vec![pubkey.to_string()], ..Default::default() };
    match source.query(vec![filter], Vec::new()).await.into_iter().max_by_key(|e| e.created_at) {
        Some(list) => vet_relay_hints(&relay_list_tags(&list).1).await,
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_what_it_can() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let profile = |author: &str| Filter { kinds: vec![0], authors: vec![author.to_string()], limit: Some(1), ..Default::default() };
        let notes = Filter { kinds: vec![1], authors: vec![a.clone()], limit: Some(5), ..Default::default() };
        let merged = merge([profile(&a), profile(&b), profile(&a), notes.clone(), notes.clone()].into_iter());
        assert_eq!(merged, vec![notes, Filter { kinds: vec![0], authors: vec![a, b], ..Default::default() }]);
    }
}
