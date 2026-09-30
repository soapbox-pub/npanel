//! Open visits, held in memory the way Plausible's session cache holds them.
//!
//! Events are matched to a session here rather than in the database, which is
//! written to only in batches: a visit is one row rewritten as it goes, and
//! asking SQLite for it on every pageview would put a read on the hot path for
//! no gain.

use indexmap::IndexMap;

/// What a session carries from the event that began it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionAttributes {
    pub referrer: String,
    pub source: String,
    pub channel: String,
    pub utm_medium: String,
    pub utm_source: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub utm_term: String,
    pub click_id_param: String,
    /// ISO 3166-1 alpha-2, or empty.
    pub country: String,
    /// The region's English name, or empty.
    pub region: String,
    /// The city's English name, or empty.
    pub city: String,
    pub device: String,
    pub browser: String,
    pub browser_version: String,
    pub os: String,
    pub os_version: String,
}

/// One visit: everything a visitor did on a site until they were gone half an
/// hour.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: i64,
    pub host_id: i64,
    pub user_id: i64,
    /// Epoch seconds of the first event.
    pub start: i64,
    /// Epoch seconds of the latest event, which is what keeps it alive.
    pub timestamp: i64,
    pub entry_page: String,
    pub exit_page: String,
    pub pageviews: i64,
    pub events: i64,
    /// Seconds from the first event to the latest.
    pub duration: i64,
    pub is_bounce: bool,
    pub attributes: SessionAttributes,
}

/// How long a visitor may be idle before their next event starts a new visit.
/// Plausible compares whole minutes and allows thirty of them, so anything
/// short of thirty-one minutes still counts.
pub const SESSION_TIMEOUT: i64 = 31 * 60;

/// Most sessions held at once. Past it the stalest session is forgotten — a
/// miscount, and a far smaller one than running out of memory.
const MAX_SESSIONS: usize = 100_000;

/// The sessions still open, keyed by site and visitor, in order of last
/// activity.
#[derive(Default)]
pub struct SessionCache {
    sessions: IndexMap<(i64, i64), Session>,
}

impl SessionCache {
    /// The open session of one visitor on one site, if they were seen
    /// recently enough.
    pub fn find(&self, host_id: i64, user_id: i64, now: i64) -> Option<&Session> {
        self.sessions.get(&(host_id, user_id)).filter(|s| now - s.timestamp < SESSION_TIMEOUT)
    }

    /// Remember a session under the visitor it belongs to.
    pub fn put(&mut self, session: Session) {
        let key = (session.host_id, session.user_id);
        // Re-inserted at the end so the map stays in order of last activity,
        // which is what lets both the sweep and the cap stop at the first live
        // entry.
        self.sessions.shift_remove(&key);
        self.sessions.insert(key, session);

        if self.sessions.len() > MAX_SESSIONS {
            self.sessions.shift_remove_index(0);
        }
    }

    /// Forget every session that can no longer be continued.
    pub fn sweep(&mut self, now: i64) {
        let expired = self.sessions.values().take_while(|s| now - s.timestamp >= SESSION_TIMEOUT).count();
        self.sessions.drain(..expired);
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(user_id: i64, timestamp: i64) -> Session {
        Session {
            id: user_id,
            host_id: 1,
            user_id,
            start: timestamp,
            timestamp,
            entry_page: "/".into(),
            exit_page: "/".into(),
            pageviews: 1,
            events: 1,
            duration: 0,
            is_bounce: true,
            attributes: SessionAttributes::default(),
        }
    }

    #[test]
    fn expires_and_sweeps() {
        let mut cache = SessionCache::default();
        cache.put(session(1, 0));
        cache.put(session(2, 1000));
        assert!(cache.find(1, 1, SESSION_TIMEOUT - 1).is_some());
        assert!(cache.find(1, 1, SESSION_TIMEOUT).is_none());
        cache.sweep(SESSION_TIMEOUT + 10);
        assert_eq!(cache.len(), 1);
        assert!(cache.find(1, 2, SESSION_TIMEOUT + 10).is_some());
    }
}
