//! Getting what a card is made of: events from relays, batched, and the
//! pubkey a NIP-05 name stands for.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{join_all, BoxFuture};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::card::{card, profile_card};
use super::entity::Entity;
use super::kinds;
use super::store::Outcome;
use crate::db::Db;
use crate::manifest::relay_list_tags;
use crate::net::PublicResolver;
use crate::nostr::pool::{Filter, Pool};
use crate::nostr::service::{dedupe, vet_relay_hints};
use crate::nostr::{is_hex64, Event};
use crate::settings::Settings;

/// How long lookups wait for each other before going out as one query.
const BATCH_WINDOW: Duration = Duration::from_millis(50);
/// The most lookups one query carries.
const MAX_BATCH: usize = 200;
/// The most relays a batch adds to the configured ones, from the hints its
/// lookups carry.
const MAX_HINTED_RELAYS: usize = 16;
/// How long one query waits on relays.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// The most of sites' filters one query carries. Relays cap the filters in a
/// subscription, and a crawl of a site with queries brings hundreds.
const MAX_SITE_FILTERS: usize = 16;

/// How long a NIP-05 document may take, and how large it may be.
const NIP05_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_NIP05_BYTES: usize = 64 * 1024;

const PROFILE_KIND: i64 = 0;
const RELAY_LIST_KIND: i64 = 10002;

/// One event a card needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// An author's kind 0.
    Profile(String),
    /// An author's kind 10002.
    RelayList(String),
    Event(String),
    Address { kind: u32, pubkey: String, identifier: String },
}

impl Want {
    pub fn matches(&self, event: &Event) -> bool {
        match self {
            Want::Profile(pubkey) => event.kind == PROFILE_KIND && event.pubkey == *pubkey,
            Want::RelayList(pubkey) => event.kind == RELAY_LIST_KIND && event.pubkey == *pubkey,
            Want::Event(id) => event.id == *id,
            Want::Address { kind, pubkey, identifier } => {
                event.kind == i64::from(*kind) && event.pubkey == *pubkey && event.tag("d").unwrap_or("") == identifier
            }
        }
    }
}

/// Where events come from. A trait so tests can stand in for the relays.
pub trait EventSource: Send + Sync {
    /// The newest event `want` names, from the configured relays and `relays`.
    fn get(&self, want: Want, relays: Vec<String>) -> BoxFuture<'_, Option<Event>>;
    /// Every event matching any of `filters`, from the configured relays and
    /// `relays`: of a replaceable or addressable event, only the newest.
    fn query(&self, filters: Vec<Filter>, relays: Vec<String>) -> BoxFuture<'_, Vec<Event>>;
}

enum Ask {
    Want { want: Want, relays: Vec<String>, reply: oneshot::Sender<Option<Event>> },
    Filters { filters: Vec<Filter>, relays: Vec<String>, reply: oneshot::Sender<Vec<Event>> },
}

impl Ask {
    fn relays(&self) -> &[String] {
        match self {
            Ask::Want { relays, .. } | Ask::Filters { relays, .. } => relays,
        }
    }
}

/// Lookups that arrive within [`BATCH_WINDOW`] of each other share one query
/// to each relay. A crawl asks for hundreds of entities at once, and a relay
/// sees a handful of subscriptions rather than a subscription apiece — which
/// is what keeps npanel welcome at the relays its deploys also depend on.
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
    fn get(&self, want: Want, relays: Vec<String>) -> BoxFuture<'_, Option<Event>> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            self.asks.send(Ask::Want { want, relays, reply }).await.ok()?;
            answer.await.ok().flatten()
        })
    }

    fn query(&self, filters: Vec<Filter>, relays: Vec<String>) -> BoxFuture<'_, Vec<Event>> {
        Box::pin(async move {
            let (reply, answer) = oneshot::channel();
            if self.asks.send(Ask::Filters { filters, relays, reply }).await.is_err() {
                return Vec::new();
            }
            answer.await.unwrap_or_default()
        })
    }
}

/// One query for a whole batch, answered lookup by lookup.
async fn query(pool: Pool, configured: Vec<String>, batch: Vec<Ask>) {
    let hinted = dedupe(batch.iter().flat_map(|ask| ask.relays().iter().cloned())).into_iter().take(MAX_HINTED_RELAYS);
    let relays = dedupe(configured.into_iter().chain(hinted));

    let mut profiles = BTreeSet::new();
    let mut relay_lists = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut addresses: BTreeMap<u32, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    // Sites' own filters, each once however many lookups asked it.
    let mut site_filters: Vec<Filter> = Vec::new();
    let mut seen = HashSet::new();
    for ask in &batch {
        let want = match ask {
            Ask::Want { want, .. } => want,
            Ask::Filters { filters, .. } => {
                for filter in filters {
                    if seen.insert(filter.to_json().to_string()) {
                        site_filters.push(filter.clone());
                    }
                }
                continue;
            }
        };
        match want {
            Want::Profile(pubkey) => {
                profiles.insert(pubkey.clone());
            }
            Want::RelayList(pubkey) => {
                relay_lists.insert(pubkey.clone());
            }
            Want::Event(id) => {
                ids.insert(id.clone());
            }
            Want::Address { kind, pubkey, identifier } => {
                let (authors, identifiers) = addresses.entry(*kind).or_default();
                authors.insert(pubkey.clone());
                identifiers.insert(identifier.clone());
            }
        }
    }

    let mut filters = Vec::new();
    for (kind, authors) in [(PROFILE_KIND, profiles), (RELAY_LIST_KIND, relay_lists)] {
        if !authors.is_empty() {
            filters.push(Filter { kinds: vec![kind], authors: authors.into_iter().collect(), ..Default::default() });
        }
    }
    if !ids.is_empty() {
        filters.push(Filter { ids: ids.into_iter().collect(), ..Default::default() });
    }
    // One filter per kind, by every author and identifier asked for: it can
    // match a pairing nobody asked for, which is sorted out below, and costs
    // a relay one subscription rather than one per address. A replaceable
    // event has no `d` tag for a relay to match, so its address is just
    // the author's.
    for (kind, (authors, identifiers)) in addresses {
        let d = if replaceable(kind) { Vec::new() } else { identifiers.into_iter().collect() };
        filters.push(Filter { kinds: vec![i64::from(kind)], authors: authors.into_iter().collect(), d, ..Default::default() });
    }

    let mut queries = Vec::new();
    if !filters.is_empty() {
        queries.push(filters);
    }
    queries.extend(site_filters.chunks(MAX_SITE_FILTERS).map(<[Filter]>::to_vec));
    let events: Vec<Event> = if relays.is_empty() {
        Vec::new()
    } else {
        let cancel = CancellationToken::new();
        join_all(queries.into_iter().map(|filters| pool.query(filters, &relays, QUERY_TIMEOUT, &cancel))).await.into_iter().flatten().collect()
    };
    for ask in batch {
        match ask {
            Ask::Want { want, reply, .. } => {
                let _ = reply.send(events.iter().filter(|event| want.matches(event)).max_by_key(|event| event.created_at).cloned());
            }
            Ask::Filters { filters, reply, .. } => {
                let mut ids = HashSet::new();
                let found = events.iter().filter(|event| filters.iter().any(|f| f.matches(event)) && ids.insert(&event.id)).cloned().collect();
                let _ = reply.send(found);
            }
        }
    }
}

/// Whether a kind is replaceable: one event per author, by kind alone. NIP-01
/// counts 0 and 3 among them, from before there was a range for them.
fn replaceable(kind: u32) -> bool {
    kind == 0 || kind == 3 || (10_000..20_000).contains(&kind)
}

/// What a NIP-05 name stands for.
pub struct Names {
    db: Db,
    http: reqwest::Client,
}

impl Names {
    pub fn new(db: Db) -> Names {
        // The domain is whoever's a crawler asked about, so it is held to the
        // rule blob downloads follow: public addresses only, and a redirect is
        // not followed anywhere.
        let http = reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicResolver))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("npanel/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(NIP05_TIMEOUT)
            .timeout(NIP05_TIMEOUT)
            .build()
            .expect("an HTTP client");
        Names { db, http }
    }

    /// The pubkey `name@domain` stands for, and the relays it names. A name
    /// one of this gateway's own hosts answers for is read from its database
    /// rather than fetched from itself.
    pub async fn resolve(&self, name: &str, domain: &str) -> Option<(String, Vec<String>)> {
        let (n, d) = (name.to_string(), domain.to_string());
        let local = self
            .db
            .call(move |conn| {
                use rusqlite::OptionalExtension;
                Ok(conn
                    .prepare_cached(
                        "SELECT nip05_names.pubkey, nip05_names.relays FROM nip05_names JOIN hosts ON hosts.id = nip05_names.host_id
                         WHERE hosts.hostname = ? AND hosts.enabled = 1 AND nip05_names.name = ?",
                    )?
                    .query_row(rusqlite::params![d, n], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .optional()?)
            })
            .await
            .ok()
            .flatten();
        if let Some((pubkey, relays)) = local {
            return Some((pubkey, crate::identifiers::parse_relay_list(&relays)));
        }

        let mut url = url::Url::parse(&format!("https://{domain}/.well-known/nostr.json")).ok()?;
        url.query_pairs_mut().append_pair("name", name);
        let mut response = self.http.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_NIP05_BYTES {
                return None;
            }
        }
        read_nip05(&body, name)
    }
}

/// A name's pubkey and relays, from a NIP-05 document.
fn read_nip05(body: &[u8], name: &str) -> Option<(String, Vec<String>)> {
    let document: serde_json::Value = serde_json::from_slice(body).ok()?;
    let pubkey = document.get("names")?.get(name)?.as_str()?.to_lowercase();
    if !is_hex64(&pubkey) {
        return None;
    }
    let relays = document
        .get("relays")
        .and_then(|relays| relays.get(&pubkey))
        .and_then(|relays| relays.as_array())
        .map(|relays| relays.iter().filter_map(|r| r.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some((pubkey, relays))
}

/// What there is to show for an entity: its card, or that it is of a kind
/// that isn't previewed, or that nothing could be found for it.
///
/// Configured relays and the entity's own hints are asked first. An author
/// who could not be found there is looked for on the relays they say they
/// write to, which costs a round trip more and is only paid for what is
/// otherwise missing.
pub async fn fetch(source: &dyn EventSource, names: &Names, entity: &Entity) -> Outcome {
    match entity {
        Entity::Profile { pubkey, relays } => profile(source, pubkey, relays).await,
        Entity::Nip05 { name, domain } => match names.resolve(name, domain).await {
            Some((pubkey, relays)) => profile(source, &pubkey, &relays).await,
            None => Outcome::NotFound,
        },
        Entity::Event { id, author, relays, .. } => {
            let hints = vet_relay_hints(relays).await;
            let want = Want::Event(id.clone());
            let (event, profile) = match author {
                Some(author) => tokio::join!(source.get(want.clone(), hints.clone()), source.get(Want::Profile(author.clone()), hints.clone())),
                None => (source.get(want.clone(), hints.clone()).await, None),
            };
            let event = match (event, author) {
                (Some(event), _) => Some(event),
                (None, Some(author)) => source.get(want, outbox(source, author).await).await,
                (None, None) => None,
            };
            match event {
                Some(event) => finish(source, &event, profile, hints).await,
                None => Outcome::NotFound,
            }
        }
        Entity::Address { kind, pubkey, identifier, relays } => {
            let hints = vet_relay_hints(relays).await;
            let want = Want::Address { kind: *kind, pubkey: pubkey.clone(), identifier: identifier.clone() };
            let (event, profile) = tokio::join!(source.get(want.clone(), hints.clone()), source.get(Want::Profile(pubkey.clone()), hints.clone()));
            let event = match event {
                Some(event) => Some(event),
                None => source.get(want, outbox(source, pubkey).await).await,
            };
            match event {
                Some(event) => finish(source, &event, profile, hints).await,
                None => Outcome::NotFound,
            }
        }
    }
}

/// An event's card, with the profile of whoever it is attributed to: the
/// one fetched beside it when that is theirs, or theirs fetched now — a
/// repost's is its original author's, a zap's its sender's.
async fn finish(source: &dyn EventSource, event: &Event, profile: Option<Event>, hints: Vec<String>) -> Outcome {
    if event.kind == 0 {
        return Outcome::Card(Arc::new(profile_card(event)));
    }
    let Some(parts) = kinds::read(event) else { return Outcome::Unsupported(event.kind) };
    let profile = match profile.filter(|p| p.pubkey == parts.author) {
        Some(profile) => Some(profile),
        None => source.get(Want::Profile(parts.author.clone()), hints).await,
    };
    Outcome::Card(Arc::new(card(event.kind, parts, profile.as_ref())))
}

async fn profile(source: &dyn EventSource, pubkey: &str, relays: &[String]) -> Outcome {
    let want = Want::Profile(pubkey.to_string());
    let event = match source.get(want.clone(), vet_relay_hints(relays).await).await {
        Some(event) => Some(event),
        None => source.get(want, outbox(source, pubkey).await).await,
    };
    match event {
        Some(event) => Outcome::Card(Arc::new(profile_card(&event))),
        None => Outcome::NotFound,
    }
}

/// The relays an author writes to, as their kind 10002 says.
pub(super) async fn outbox(source: &dyn EventSource, pubkey: &str) -> Vec<String> {
    match source.get(Want::RelayList(pubkey.to_string()), Vec::new()).await {
        Some(list) => vet_relay_hints(&relay_list_tags(&list).1).await,
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wants() {
        let secret = "0000000000000000000000000000000000000000000000000000000000000003";
        let article = Event::sign(secret, 1, 30023, vec![vec!["d".into(), "post".into()]], "");
        let want = |identifier: &str| Want::Address { kind: 30023, pubkey: article.pubkey.clone(), identifier: identifier.into() };
        assert!(want("post").matches(&article));
        assert!(!want("other").matches(&article));
        assert!(Want::Event(article.id.clone()).matches(&article));
        assert!(!Want::Profile(article.pubkey.clone()).matches(&article));
    }

    #[test]
    fn reads_nip05_documents() {
        let pubkey = "a".repeat(64);
        let body = format!(r#"{{"names":{{"bob":"{}"}},"relays":{{"{pubkey}":["wss://r.example.com"]}}}}"#, pubkey.to_uppercase());
        assert_eq!(read_nip05(body.as_bytes(), "bob"), Some((pubkey.clone(), vec!["wss://r.example.com".into()])));
        assert_eq!(read_nip05(body.as_bytes(), "alice"), None);
        assert_eq!(read_nip05(br#"{"names":{"bob":"nope"}}"#, "bob"), None);
        assert_eq!(read_nip05(b"<html>", "bob"), None);
    }
}
