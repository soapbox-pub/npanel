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
    /// Epoch seconds of the latest event that counts in it — engagement
    /// pings don't — as it is stored.
    pub timestamp: i64,
    /// Epoch seconds of the latest event of any kind, which is what keeps it
    /// open. Only memory needs it.
    pub last_seen: i64,
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
    /// Whether one visitor on one site has a session still open.
    pub fn is_open(&self, host_id: i64, user_id: i64, now: i64) -> bool {
        self.get(host_id, user_id).is_some_and(|s| now - s.last_seen < SESSION_TIMEOUT)
    }

    pub fn get(&self, host_id: i64, user_id: i64) -> Option<&Session> {
        self.sessions.get(&(host_id, user_id))
    }

    /// A session to continue, moved to the end — the map is kept in order of
    /// last activity, which is what lets both the sweep and the cap stop at
    /// the first live entry.
    pub fn continue_session(&mut self, host_id: i64, user_id: i64) -> Option<&mut Session> {
        let index = self.sessions.get_index_of(&(host_id, user_id))?;
        let last = self.sessions.len() - 1;
        self.sessions.move_index(index, last);
        self.sessions.get_index_mut(last).map(|(_, session)| session)
    }

    /// Remember a new session under the visitor it belongs to. Returns the
    /// stalest session if the cache was full and forgot it.
    pub fn put(&mut self, session: Session) -> (&mut Session, Option<Session>) {
        let key = (session.host_id, session.user_id);
        self.sessions.shift_remove(&key);
        let evicted = if self.sessions.len() >= MAX_SESSIONS { self.sessions.shift_remove_index(0).map(|(_, s)| s) } else { None };
        let (index, _) = self.sessions.insert_full(key, session);
        (&mut self.sessions[index], evicted)
    }

    /// Forget every session that can no longer be continued.
    pub fn sweep(&mut self, now: i64) {
        let expired = self.sessions.values().take_while(|s| now - s.last_seen >= SESSION_TIMEOUT).count();
        self.sessions.drain(..expired);
    }

    #[cfg(test)]
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
            last_seen: timestamp,
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
        assert!(cache.is_open(1, 1, SESSION_TIMEOUT - 1));
        assert!(!cache.is_open(1, 1, SESSION_TIMEOUT));
        cache.sweep(SESSION_TIMEOUT + 10);
        assert_eq!(cache.len(), 1);
        assert!(cache.is_open(1, 2, SESSION_TIMEOUT + 10));
    }

    #[test]
    fn continuing_moves_to_the_end() {
        let mut cache = SessionCache::default();
        cache.put(session(1, 0));
        cache.put(session(2, 10));
        cache.continue_session(1, 1).unwrap().last_seen = 2000;
        // The sweep stops at the first live entry, so order matters.
        cache.sweep(SESSION_TIMEOUT + 20);
        assert_eq!(cache.len(), 1);
        assert!(cache.get(1, 1).is_some());
    }
}
