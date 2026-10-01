//! Plausible's ingestion: validate an event, decide which site and visit it
//! belongs to, and record it.
//!
//! The answers are Plausible's too. An event recorded is a `202 ok`. One that
//! was well-formed but isn't recorded — a bot, a site that isn't counting, an
//! engagement ping for a visit that already ended — is still a `202`, with
//! `x-plausible-dropped` saying how many were dropped, because nothing the
//! sender could change would make it count. Only a malformed request is a 400.

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;
use rand::Rng;

use super::channel::{channel, ChannelInputs};
use super::geo::{Geo, Location};
use super::request::{parse_event_request, EventRequest};
use super::salts::Salts;
use super::request::RequestErrors;
use super::sessions::{Session, SessionAttributes, SessionCache};
use super::source::{acquisition, is_spam_referrer, query_param};
use super::store::{AnalyticsStore, EventKind, EventRow};
use super::user_agent::{Device, UserAgents};

/// Ad click ids a landing URL may carry, in the order Plausible checks them.
const CLICK_IDS: [&str; 6] = ["gclid", "gbraid", "wbraid", "msclkid", "fbclid", "twclid"];

/// A site events may be recorded for.
#[derive(Clone, Debug)]
pub struct AnalyticsHost {
    pub id: i64,
    pub hostname: String,
    /// Whether it is enabled and counting visits: the only hosts events are
    /// kept for.
    pub counting: bool,
}

/// Who sent an event, as far as the request says.
pub struct Sender<'a> {
    pub ip: &'a str,
    pub user_agent: &'a str,
}

/// Events held before a flush is forced, rather than waiting for the timer.
const FLUSH_AT: usize = 5_000;

/// The most ever held. New events are dropped past it rather than old ones,
/// because an old event is one a visitor was already told was counted.
const MAX_BUFFERED: usize = 100_000;

/// What happened to one request.
pub enum Received {
    /// Well-formed, and `dropped` of the sites it named didn't keep it.
    Accepted { dropped: usize },
    Invalid(RequestErrors),
}

/// What is waiting to be written.
#[derive(Default)]
pub struct Pending {
    pub events: Vec<EventRow>,
    pub sessions: Vec<Session>,
    /// Events dropped because the buffer was full.
    pub dropped: usize,
}

/// Everything recording an event touches. Used one event at a time, and only
/// in memory: the store is written by whoever takes the pending writes.
pub struct Ingest {
    pub sessions: SessionCache,
    pub salts: Salts,
    pub user_agents: UserAgents,
    pub geo: Geo,
    events: Vec<EventRow>,
    /// Sessions changed since the last flush, by site and visitor. Written
    /// once each, however often they changed in between.
    dirty: HashSet<(i64, i64)>,
    /// Changed sessions the cache had to forget before they were written.
    evicted: Vec<Session>,
    dropped: usize,
}

impl Ingest {
    pub fn new(sessions: SessionCache, geo: Geo) -> Ingest {
        Ingest {
            sessions,
            salts: Salts::default(),
            user_agents: UserAgents::default(),
            geo,
            events: Vec::new(),
            dirty: HashSet::new(),
            evicted: Vec::new(),
            dropped: 0,
        }
    }

    /// Take everything waiting to be written.
    pub fn take_pending(&mut self) -> Pending {
        let mut sessions = std::mem::take(&mut self.evicted);
        sessions.extend(self.dirty.drain().filter_map(|(host_id, user_id)| self.sessions.get(host_id, user_id).cloned()));
        Pending { events: std::mem::take(&mut self.events), sessions, dropped: std::mem::take(&mut self.dropped) }
    }

    /// Handle one request body. `hosts` names the sites by hostname, and
    /// `site` is the host whose hostname the request arrived on: the only one
    /// it may record for. The tracker always reports to its own site, and
    /// without this anyone could fill any site's figures from anywhere. The
    /// flag is whether the buffer asked to be written now.
    pub fn handle(
        &mut self,
        store: &Mutex<AnalyticsStore>,
        body: &str,
        sender: &Sender,
        hosts: &HashMap<String, AnalyticsHost>,
        site: Option<i64>,
        now: i64,
    ) -> (Received, bool) {
        let event = match parse_event_request(body) {
            Ok(event) => event,
            Err(errors) => return (Received::Invalid(errors), false),
        };

        let mut dropped = 0;
        let mut flush = false;

        // Spam is spam whichever site it was sent to.
        if is_spam_referrer(event.referrer.as_deref()) {
            dropped = event.domains.len();
        } else {
            for domain in &event.domains {
                // `d` arrives without `www.`, which a site configured under its
                // `www` name still has to be found by.
                let host = hosts.get(domain).or_else(|| hosts.get(&format!("www.{domain}"))).filter(|h| Some(h.id) == site);
                match self.record(store, &event, domain, host, sender, now) {
                    Some(full) => flush |= full,
                    None => dropped += 1,
                }
            }
        }

        (Received::Accepted { dropped }, flush)
    }

    /// Record one event for one site. `None` when it wasn't kept; otherwise
    /// whether the buffer is full enough to flush.
    fn record(
        &mut self,
        store: &Mutex<AnalyticsStore>,
        event: &EventRequest,
        domain: &str,
        host: Option<&AnalyticsHost>,
        sender: &Sender,
        now: i64,
    ) -> Option<bool> {
        let host = host.filter(|h| h.counting)?;

        let device = if sender.user_agent.is_empty() { Some(None) } else { self.user_agents.parse(sender.user_agent).map(Some) };
        // A bot is never counted.
        let device = device?;

        let url_host = event.url.host_str().unwrap_or("");
        let root_domain = crate::psl::domain(url_host).filter(|d| !d.is_empty()).unwrap_or(if url_host.is_empty() { "(none)" } else { url_host });
        let (current, previous) = self.salts.user_ids(store, now, &[sender.user_agent, sender.ip, domain, root_domain]).ok()?;

        // A visit continued under yesterday's salt stays filed under it.
        let open = [Some(current), previous].into_iter().flatten().find(|user| self.sessions.is_open(host.id, *user, now));
        let pageview = event.name == "pageview";
        let engagement = event.name == "engagement";

        let session = match open {
            Some(user) => {
                let session = self.sessions.continue_session(host.id, user)?;
                session.last_seen = now;
                // Engagement only ever continues a visit: it keeps the session
                // alive and is stored against it, but changes nothing it counts.
                if !engagement {
                    session.timestamp = now;
                    session.duration = (now - session.start).abs();
                    session.events += 1;
                    if pageview {
                        session.pageviews += 1;
                        if session.entry_page.is_empty() {
                            session.entry_page = event.pathname.clone();
                        }
                        session.exit_page = event.pathname.clone();
                    }
                    // A bounce stops being one on a second pageview or anything
                    // the visitor actively did, and never becomes one again.
                    if session.is_bounce {
                        session.is_bounce = !(session.pageviews >= 2 || (event.interactive && !pageview));
                    }
                }
                session
            }
            None if engagement => return None,
            None => {
                let location = self.geo.lookup(sender.ip);
                let (session, evicted) = self.sessions.put(Session {
                    id: rand::thread_rng().gen_range(0..(1i64 << 48) - 1),
                    host_id: host.id,
                    user_id: current,
                    start: now,
                    timestamp: now,
                    last_seen: now,
                    entry_page: if pageview { event.pathname.clone() } else { String::new() },
                    exit_page: if pageview { event.pathname.clone() } else { String::new() },
                    pageviews: i64::from(pageview),
                    events: 1,
                    duration: 0,
                    is_bounce: pageview || !event.interactive,
                    attributes: attributes(event, device.as_ref(), location),
                });
                if let Some(evicted) = evicted.filter(|s| self.dirty.remove(&(s.host_id, s.user_id))) {
                    self.evicted.push(evicted);
                }
                session
            }
        };
        let (session_id, user_id) = (session.id, session.user_id);

        // An engagement ping only moved `last_seen`, which isn't written.
        if !engagement {
            self.dirty.insert((host.id, user_id));
        }

        let kind = if pageview {
            EventKind::Pageview
        } else if engagement {
            EventKind::Engagement
        } else {
            EventKind::Custom
        };

        if self.events.len() >= MAX_BUFFERED {
            self.dropped += 1;
            return Some(false);
        }
        self.events.push(EventRow {
            host_id: host.id,
            timestamp: now,
            kind,
            session_id,
            user_id,
            pathname: event.pathname.clone(),
            engagement_time: event.engagement_time,
            scroll_depth: event.scroll_depth,
        });
        Some(self.events.len() >= FLUSH_AT)
    }
}

/// Longest attribution value a session keeps. Each is the sender's to choose,
/// and an open session sits in memory for its whole visit.
const MAX_ATTRIBUTE: usize = 300;

fn clip(mut value: String) -> String {
    if let Some((at, _)) = value.char_indices().nth(MAX_ATTRIBUTE) {
        value.truncate(at);
    }
    value
}

/// A session's attributes, all taken from the event that began it.
fn attributes(event: &EventRequest, device: Option<&Device>, location: Location) -> SessionAttributes {
    let url = &event.url;
    let found = acquisition(url, event.referrer.as_deref());
    let click_id_param = CLICK_IDS.iter().find(|param| url.query_pairs().any(|(k, _)| k == **param)).copied().unwrap_or("");

    let mut utm_medium = query_param(url, "utm_medium").unwrap_or_default();
    // An auto-tagged ad click says what it was without a medium to say it.
    if utm_medium.is_empty() && found.source == "Google" && click_id_param == "gclid" {
        utm_medium = "(gclid)".into();
    }
    if utm_medium.is_empty() && found.source == "Bing" && click_id_param == "msclkid" {
        utm_medium = "(msclkid)".into();
    }

    let utm_source = ["utm_source", "source", "ref"]
        .iter()
        .find_map(|name| query_param(url, name).filter(|v| !v.is_empty()))
        .unwrap_or_default();
    let utm_campaign = query_param(url, "utm_campaign").unwrap_or_default();

    let channel = channel(ChannelInputs {
        source: &found.source,
        utm_medium: &utm_medium,
        utm_campaign: &utm_campaign,
        utm_source: &utm_source,
        click_id_param,
    })
    .to_string();

    let device = device.cloned().unwrap_or_default();
    SessionAttributes {
        referrer: clip(found.referrer),
        source: clip(found.source),
        channel,
        utm_medium: clip(utm_medium),
        utm_source: clip(utm_source),
        utm_campaign: clip(utm_campaign),
        utm_content: clip(query_param(url, "utm_content").unwrap_or_default()),
        utm_term: clip(query_param(url, "utm_term").unwrap_or_default()),
        click_id_param: click_id_param.to_string(),
        country: location.country,
        region: location.region,
        city: location.city,
        device: device.device,
        browser: device.browser,
        browser_version: device.browser_version,
        os: device.os,
        os_version: device.os_version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

    fn dropped(received: &Received) -> Option<usize> {
        match received {
            Received::Accepted { dropped } => Some(*dropped),
            Received::Invalid(_) => None,
        }
    }

    #[test]
    fn records_visits() {
        let dir = tempfile::tempdir().unwrap();
        let store = Mutex::new(AnalyticsStore::new(rusqlite::Connection::open(dir.path().join("a.sqlite3")).unwrap()).unwrap());
        let mut ingest = Ingest::new(SessionCache::default(), Geo::new(dir.path(), crate::log::Logger::new(crate::log::LogLevel::Error)));
        let hosts: HashMap<String, AnalyticsHost> = [
            ("docs.example.com".into(), AnalyticsHost { id: 1, hostname: "docs.example.com".into(), counting: true }),
            ("off.example.com".into(), AnalyticsHost { id: 2, hostname: "off.example.com".into(), counting: false }),
        ]
        .into();
        let sender = Sender { ip: "203.0.113.5", user_agent: UA };
        let mut handle_on = |site: i64, body: &str, sender: &Sender, now: i64| ingest.handle(&store, body, sender, &hosts, Some(site), now).0;

        // Sent to one site naming another: not kept.
        let received = handle_on(2, r#"{"n":"pageview","u":"https://docs.example.com/","d":"docs.example.com"}"#, &sender, 990);
        assert_eq!(dropped(&received), Some(1));
        let mut handle = |body: &str, sender: &Sender, now: i64| handle_on(1, body, sender, now);

        let received = handle(r#"{"n":"pageview","u":"https://docs.example.com/?utm_source=google","d":"docs.example.com","r":null}"#, &sender, 1000);
        assert_eq!(dropped(&received), Some(0));
        let received = handle(r#"{"n":"pageview","u":"https://docs.example.com/about","d":"docs.example.com"}"#, &sender, 1010);
        assert_eq!(dropped(&received), Some(0));
        // Keeps the visit open without moving what is stored.
        let received = handle(r#"{"n":"engagement","u":"https://docs.example.com/about","d":"docs.example.com","e":5000,"sd":40}"#, &sender, 1020);
        assert_eq!(dropped(&received), Some(0));
        let received = handle(r#"{"n":"pageview","u":"https://off.example.com/","d":"off.example.com"}"#, &sender, 1030);
        assert_eq!(dropped(&received), Some(1));
        let bot = Sender { ip: "203.0.113.5", user_agent: "Googlebot/2.1" };
        let received = handle(r#"{"n":"pageview","u":"https://docs.example.com/","d":"docs.example.com"}"#, &bot, 1040);
        assert_eq!(dropped(&received), Some(1));
        assert!(dropped(&handle("{", &sender, 1050)).is_none());

        let pending = ingest.take_pending();
        assert_eq!((pending.events.len(), pending.sessions.len()), (3, 1));
        store.lock().write(&pending.events, &pending.sessions).unwrap();
        assert_eq!(ingest.take_pending().events.len(), 0);

        let sessions = store.lock().open_sessions(0).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!((s.pageviews, s.is_bounce, s.duration, s.timestamp), (2, false, 10, 1010));
        assert_eq!((s.entry_page.as_str(), s.exit_page.as_str()), ("/", "/about"));
        assert_eq!(s.attributes.source, "Google");
        assert_eq!(s.attributes.channel, "Organic Search");
        assert_eq!(s.attributes.browser, "Chrome");
    }
}
