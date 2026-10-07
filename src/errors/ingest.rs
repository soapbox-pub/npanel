//! Receiving a report: which site it is for, whether it is kept, and what is
//! kept of it.

use parking_lot::Mutex;

use super::envelope::{parse_envelope, read_event};
use super::grouping::group;
use super::store::{compress, ErrorsStore, Occurrence};
use super::symbolicate::Symbolicator;
use crate::analytics::salts::Salts;
use crate::analytics::user_agent::UserAgents;
use crate::fields;
use crate::log::{with_err, Logger};
use crate::rate_limit::{RateLimit, RateLimiter};

/// A site reports may be kept for.
#[derive(Clone, Debug)]
pub struct ErrorsHost {
    pub id: i64,
    pub hostname: String,
    /// Whether it is enabled and has error reporting on: the only hosts
    /// reports are kept for.
    pub collecting: bool,
}

/// Who sent a report, as far as the request says.
pub struct Sender<'a> {
    pub ip: &'a str,
    pub user_agent: &'a str,
}

/// Reports one site may send, and how fast the allowance comes back. A
/// page that throws on every frame of an animation would otherwise send
/// thousands from each visitor; a few from each is all anyone reads.
const PER_SITE: RateLimit = RateLimit { capacity: 60.0, interval_ms: 60_000.0 };
/// Sites tracked by the rate limit at once.
const MAX_LIMITED_SITES: usize = 10_000;

/// Bytes of reports held before a flush is forced, rather than waiting for
/// the timer.
const FLUSH_AT: usize = 4 * 1024 * 1024;
/// The most ever held. New reports are dropped past it.
const MAX_BUFFERED: usize = 32 * 1024 * 1024;

/// What happened to one request.
#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    /// Well-formed. The id is the first event's that was kept, if any was.
    Accepted(Option<String>),
    /// The site's allowance is spent.
    Limited,
    Invalid(String),
}

/// What waits to be written.
#[derive(Default)]
pub struct Pending {
    pub occurrences: Vec<Occurrence>,
    /// Reports dropped because the buffer was full.
    pub dropped: usize,
}

/// Everything receiving a report touches in memory. Held only briefly:
/// mapping frames, the slow part, happens outside it.
pub struct Ingest {
    user_agents: UserAgents,
    salts: Salts,
    limiter: RateLimiter,
    pending: Vec<Occurrence>,
    weight: usize,
    dropped: usize,
}

impl Default for Ingest {
    fn default() -> Self {
        Ingest {
            user_agents: UserAgents::default(),
            salts: Salts::default(),
            limiter: RateLimiter::new(MAX_LIMITED_SITES),
            pending: Vec::new(),
            weight: 0,
            dropped: 0,
        }
    }
}

impl Ingest {
    pub fn take_pending(&mut self) -> Pending {
        self.weight = 0;
        Pending { occurrences: std::mem::take(&mut self.pending), dropped: std::mem::take(&mut self.dropped) }
    }
}

/// Everything one report needs besides its body.
pub struct Context<'a> {
    pub ingest: &'a Mutex<Ingest>,
    pub store: &'a Mutex<ErrorsStore>,
    pub symbolicator: &'a Symbolicator,
    pub log: &'a Logger,
}

/// Handle one request body. `host` is the site whose name it arrived on —
/// the only one it may be kept for — and `hostname` that name, which the
/// site's own frames are served from. The flag is whether the buffer asked
/// to be written now. Blocking.
pub fn handle(cx: &Context, body: &[u8], sender: &Sender, host: Option<&ErrorsHost>, hostname: &str, now: i64) -> (Received, bool) {
    let events = match parse_envelope(body) {
        Ok(events) => events,
        Err(error) => return (Received::Invalid(error), false),
    };
    let Some(host) = host.filter(|h| h.collecting) else { return (Received::Accepted(None), false) };
    if events.is_empty() {
        return (Received::Accepted(None), false);
    }

    let (device, allowed) = {
        let mut ingest = cx.ingest.lock();
        let device = if sender.user_agent.is_empty() { Some(Default::default()) } else { ingest.user_agents.parse(sender.user_agent) };
        // A bot's errors are its own.
        let Some(device) = device else { return (Received::Accepted(None), false) };
        let key = host.id.to_string();
        let allowed = events.iter().take_while(|_| ingest.limiter.take(&key, PER_SITE)).count();
        (device, allowed)
    };
    if allowed == 0 {
        return (Received::Limited, false);
    }

    let mut occurrences = Vec::with_capacity(allowed);
    for event in &events[..allowed] {
        let mut report = read_event(event);
        report.browser = device.browser.clone();
        report.browser_version = device.browser_version.clone();
        report.os = device.os.clone();
        report.os_version = device.os_version.clone();
        report.device = device.device.clone();
        // A frame left unmapped is a less readable report, not a lost one.
        if let Err(error) = cx.symbolicator.symbolicate(&mut report, host.id, hostname) {
            cx.log.warn("errors_symbolicate_failed", with_err(fields! { "hostname" => host.hostname.as_str() }, error));
        }
        let grouping = group(&report);
        let payload = compress(&serde_json::to_vec(&report).unwrap_or_default());
        occurrences.push(Occurrence {
            host_id: host.id,
            grouping,
            level: report.level,
            received_at: now,
            release: report.release,
            visitor: 0,
            event_id: report.event_id,
            url: report.url,
            browser: report.browser,
            os: report.os,
            payload,
        });
    }

    let mut ingest = cx.ingest.lock();
    let visitor = match ingest.salts.user_ids(cx.store, now, &[sender.user_agent, sender.ip, &host.hostname]) {
        Ok((visitor, _)) => visitor,
        Err(error) => {
            cx.log.error("errors_salt_failed", with_err(fields! {}, error));
            return (Received::Accepted(None), false);
        }
    };
    let id = occurrences.first().map(|o| o.event_id.clone());
    for mut occurrence in occurrences {
        if ingest.weight + occurrence.weight() > MAX_BUFFERED {
            ingest.dropped += 1;
            continue;
        }
        occurrence.visitor = visitor;
        ingest.weight += occurrence.weight();
        ingest.pending.push(occurrence);
    }
    let full = ingest.weight >= FLUSH_AT;
    (Received::Accepted(id), full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blobs::BlobStore;
    use crate::db::testing::temp_db;

    const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

    #[test]
    fn keeps_what_it_should() {
        let t = temp_db();
        let store = Mutex::new(ErrorsStore::new(crate::db::open_connection(&t.dir.path().join("errors.sqlite3")).unwrap()).unwrap());
        let symbolicator = Symbolicator::new(t.db.clone(), BlobStore::new(&t.dir.path().join("blobs")));
        let ingest = Mutex::new(Ingest::default());
        let log = Logger::new(crate::log::LogLevel::Error);
        let cx = Context { ingest: &ingest, store: &store, symbolicator: &symbolicator, log: &log };
        let on = ErrorsHost { id: 1, hostname: "docs.example.com".into(), collecting: true };
        let off = ErrorsHost { id: 2, hostname: "off.example.com".into(), collecting: false };
        let sender = Sender { ip: "203.0.113.5", user_agent: UA };
        let envelope = |id: &str| format!("{{}}\n{{\"type\":\"event\"}}\n{{\"event_id\":\"{id}\",\"message\":\"boom 1\"}}\n");

        let id = "a".repeat(32);
        assert_eq!(handle(&cx, envelope(&id).as_bytes(), &sender, Some(&on), "docs.example.com", 1000).0, Received::Accepted(Some(id.clone())));
        assert_eq!(handle(&cx, envelope(&id).as_bytes(), &sender, Some(&off), "off.example.com", 1000).0, Received::Accepted(None));
        assert_eq!(handle(&cx, envelope(&id).as_bytes(), &sender, None, "nobody.example.com", 1000).0, Received::Accepted(None));
        let bot = Sender { ip: "203.0.113.5", user_agent: "Googlebot/2.1 (+http://www.google.com/bot.html)" };
        assert_eq!(handle(&cx, envelope(&id).as_bytes(), &bot, Some(&on), "docs.example.com", 1000).0, Received::Accepted(None));
        assert!(matches!(handle(&cx, b"nope", &sender, Some(&on), "docs.example.com", 1000).0, Received::Invalid(_)));

        // The site's allowance runs out.
        let limited = (0..100).filter(|_| handle(&cx, envelope(&id).as_bytes(), &sender, Some(&on), "docs.example.com", 1000).0 == Received::Limited).count();
        // Sixty a minute, so a slow run may have earned one or two back.
        assert!((35..=41).contains(&limited), "{limited}");

        let pending = ingest.lock().take_pending();
        assert_eq!(pending.occurrences.len(), 101 - limited);
        let first = &pending.occurrences[0];
        assert_eq!((first.browser.as_str(), first.os.as_str(), first.grouping.title.as_str()), ("Firefox", "GNU/Linux", "boom 1"));
        assert_eq!(pending.occurrences[1].visitor, first.visitor);
        store.lock().write(&pending.occurrences).unwrap();
    }
}
