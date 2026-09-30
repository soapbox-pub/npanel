//! Plausible's ingestion: validate an event, decide which site and visit it
//! belongs to, and record it.
//!
//! The answers are Plausible's too. An event recorded is a `202 ok`. One that
//! was well-formed but isn't recorded — a bot, a site that isn't counting, an
//! engagement ping for a visit that already ended — is still a `202`, with
//! `x-plausible-dropped` saying how many were dropped, because nothing the
//! sender could change would make it count. Only a malformed request is a 400.

use std::collections::HashMap;

use rand::Rng;

use super::channel::{channel, ChannelInputs};
use super::geo::{Geo, Location};
use super::request::{parse_event_request, EventRequest};
use super::salts::Salts;
use super::sessions::{Session, SessionAttributes, SessionCache};
use super::source::{acquisition, is_spam_referrer, query_param};
use super::store::{AnalyticsStore, EventKind, EventRow};
use super::user_agent::{Device, UserAgents};
use super::WorkerResponse;

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

/// Everything recording an event touches. Owned by the analytics thread and
/// used one event at a time.
pub struct Ingest {
    pub store: AnalyticsStore,
    pub sessions: SessionCache,
    pub salts: Salts,
    pub user_agents: UserAgents,
    pub geo: Geo,
}

impl Ingest {
    /// Handle one request body. `hosts` names the sites by hostname; `flush`
    /// is whether the buffer asked to be written now.
    pub fn handle(&mut self, body: &str, sender: &Sender, hosts: &HashMap<String, AnalyticsHost>, now: i64) -> (WorkerResponse, bool) {
        let event = match parse_event_request(body) {
            Ok(event) => event,
            Err(errors) => {
                let body = serde_json::json!({ "errors": errors }).to_string();
                return (WorkerResponse::new(400, "application/json", body), false);
            }
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
                let host = hosts.get(domain).or_else(|| hosts.get(&format!("www.{domain}")));
                match self.record(&event, domain, host, sender, now) {
                    Some(full) => flush |= full,
                    None => dropped += 1,
                }
            }
        }

        let mut response = WorkerResponse::new(202, "text/plain; charset=utf-8", "ok".into());
        if dropped > 0 {
            response.headers.push(("x-plausible-dropped".into(), dropped.to_string()));
        }
        (response, flush)
    }

    /// Record one event for one site. `None` when it wasn't kept; otherwise
    /// whether the buffer is full enough to flush.
    fn record(&mut self, event: &EventRequest, domain: &str, host: Option<&AnalyticsHost>, sender: &Sender, now: i64) -> Option<bool> {
        let host = host.filter(|h| h.counting)?;

        let device = if sender.user_agent.is_empty() { Some(None) } else { self.user_agents.parse(sender.user_agent).map(Some) };
        // A bot is never counted.
        let device = device?;

        let url_host = event.url.host_str().unwrap_or("");
        let root_domain = crate::psl::domain(url_host).filter(|d| !d.is_empty()).unwrap_or(if url_host.is_empty() { "(none)" } else { url_host });
        let (current, previous) =
            self.salts.user_ids(&self.store, now, &[sender.user_agent, sender.ip, domain, root_domain]).ok()?;

        let found = self
            .sessions
            .find(host.id, current, now)
            .or_else(|| previous.and_then(|previous| self.sessions.find(host.id, previous, now)))
            .cloned();

        let pageview = event.name == "pageview";

        let session = if event.name == "engagement" {
            // Engagement only ever continues a visit: it keeps the session
            // alive and is stored against it, but changes nothing it counts.
            let mut session = found?;
            session.timestamp = now;
            self.sessions.put(session.clone());
            session
        } else if let Some(mut session) = found {
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
            // A bounce stops being one on a second pageview or anything the
            // visitor actively did, and never becomes one again.
            if session.is_bounce {
                session.is_bounce = !(session.pageviews >= 2 || (event.interactive && !pageview));
            }
            self.sessions.put(session.clone());
            self.store.touch_session(&session);
            session
        } else {
            let location = self.geo.lookup(sender.ip);
            let session = Session {
                id: rand::thread_rng().gen_range(0..(1i64 << 48) - 1),
                host_id: host.id,
                user_id: current,
                start: now,
                timestamp: now,
                entry_page: if pageview { event.pathname.clone() } else { String::new() },
                exit_page: if pageview { event.pathname.clone() } else { String::new() },
                pageviews: i64::from(pageview),
                events: 1,
                duration: 0,
                is_bounce: pageview || !event.interactive,
                attributes: attributes(event, device.as_ref(), location),
            };
            self.sessions.put(session.clone());
            self.store.touch_session(&session);
            session
        };

        let kind = if pageview {
            EventKind::Pageview
        } else if event.name == "engagement" {
            EventKind::Engagement
        } else {
            EventKind::Custom
        };

        Some(self.store.add_event(EventRow {
            host_id: host.id,
            timestamp: now,
            kind,
            session_id: session.id,
            user_id: session.user_id,
            pathname: event.pathname.clone(),
            engagement_time: event.engagement_time,
            scroll_depth: event.scroll_depth,
        }))
    }
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
        referrer: found.referrer,
        source: found.source,
        channel,
        utm_medium,
        utm_source,
        utm_campaign,
        utm_content: query_param(url, "utm_content").unwrap_or_default(),
        utm_term: query_param(url, "utm_term").unwrap_or_default(),
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

    fn ingest(dir: &std::path::Path) -> Ingest {
        Ingest {
            store: AnalyticsStore::new(rusqlite::Connection::open(dir.join("a.sqlite3")).unwrap()).unwrap(),
            sessions: SessionCache::default(),
            salts: Salts::default(),
            user_agents: UserAgents::default(),
            geo: Geo::new(dir, crate::log::Logger::new(crate::log::LogLevel::Error)),
        }
    }

    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

    #[test]
    fn records_visits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ingest = ingest(dir.path());
        let hosts: HashMap<String, AnalyticsHost> = [
            ("docs.example.com".into(), AnalyticsHost { id: 1, hostname: "docs.example.com".into(), counting: true }),
            ("off.example.com".into(), AnalyticsHost { id: 2, hostname: "off.example.com".into(), counting: false }),
        ]
        .into();
        let sender = Sender { ip: "203.0.113.5", user_agent: UA };

        let (response, _) = ingest.handle(
            r#"{"n":"pageview","u":"https://docs.example.com/?utm_source=google","d":"docs.example.com","r":null}"#,
            &sender,
            &hosts,
            1000,
        );
        assert_eq!(response.status, 202);
        assert!(response.headers.iter().all(|(k, _)| k != "x-plausible-dropped"));

        let (response, _) = ingest.handle(r#"{"n":"pageview","u":"https://docs.example.com/about","d":"docs.example.com"}"#, &sender, &hosts, 1010);
        assert_eq!(response.status, 202);

        let (response, _) = ingest.handle(r#"{"n":"pageview","u":"https://off.example.com/","d":"off.example.com"}"#, &sender, &hosts, 1020);
        assert!(response.headers.contains(&("x-plausible-dropped".into(), "1".into())));

        let bot = Sender { ip: "203.0.113.5", user_agent: "Googlebot/2.1" };
        let (response, _) = ingest.handle(r#"{"n":"pageview","u":"https://docs.example.com/","d":"docs.example.com"}"#, &bot, &hosts, 1030);
        assert!(response.headers.contains(&("x-plausible-dropped".into(), "1".into())));

        assert_eq!(ingest.store.flush().unwrap(), (2, 1));
        let sessions = ingest.store.open_sessions(0).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!((s.pageviews, s.is_bounce, s.duration), (2, false, 10));
        assert_eq!((s.entry_page.as_str(), s.exit_page.as_str()), ("/", "/about"));
        assert_eq!(s.attributes.source, "Google");
        assert_eq!(s.attributes.channel, "Organic Search");
        assert_eq!(s.attributes.browser, "Chrome");

        let (response, _) = ingest.handle("{", &sender, &hosts, 1040);
        assert_eq!(response.status, 400);
    }
}
