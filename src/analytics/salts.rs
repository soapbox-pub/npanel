//! The daily salts visitor ids are hashed with, the way Plausible does it.
//!
//! A visitor is never stored as themselves. What is stored is a hash of their
//! user agent, address and the site, under a salt that is replaced every day
//! at midnight UTC and destroyed a day after that. The same person is the
//! same id for one day on one site, and nothing links them further.
//!
//! The previous salt is kept for one day for the sake of visits in progress: a
//! visitor browsing across midnight is found by trying the old salt as well.

use anyhow::Result;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sha2::Sha256;

use super::store::AnalyticsStore;

const DAY: i64 = 24 * 60 * 60;

#[derive(Default)]
pub struct Salts {
    current: Vec<u8>,
    previous: Option<Vec<u8>>,
    /// Epoch seconds of the midnight the current salt belongs to.
    day: i64,
}

impl Salts {
    /// Rotate if the current salt is from before today. Cheap enough to call
    /// per event: the store is only locked on the first event of a day.
    pub fn refresh(&mut self, store: &Mutex<AnalyticsStore>, now: i64) -> Result<()> {
        let today = now - now.rem_euclid(DAY);
        if self.day == today && !self.current.is_empty() {
            return Ok(());
        }

        let store = store.lock();
        let stored = store.salts()?;
        match stored.first() {
            Some((salt, created_at)) if *created_at >= today => {
                self.current = salt.clone();
                self.previous = stored.get(1).map(|(salt, _)| salt.clone());
            }
            newest => {
                self.previous = newest.map(|(salt, _)| salt.clone());
                self.current = store.rotate_salt(now)?;
            }
        }

        self.day = today;
        Ok(())
    }

    /// A visitor's id under the current salt, and under the previous one if
    /// there is one: 48 bits of an HMAC, enough that two visitors to one site
    /// on one day colliding is a one-in-millions event.
    pub fn user_ids(&mut self, store: &Mutex<AnalyticsStore>, now: i64, parts: &[&str]) -> Result<(i64, Option<i64>)> {
        self.refresh(store, now)?;
        let input = parts.concat();
        Ok((hash(&self.current, &input), self.previous.as_ref().map(|salt| hash(salt, &input))))
    }
}

fn hash(salt: &[u8], input: &str) -> i64 {
    let mut mac = Hmac::<Sha256>::new_from_slice(salt).expect("HMAC takes a key of any length");
    mac.update(input.as_bytes());
    let digest = mac.finalize().into_bytes();
    digest[..6].iter().fold(0i64, |acc, &b| (acc << 8) | i64::from(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_daily() {
        let dir = tempfile::tempdir().unwrap();
        let store = Mutex::new(AnalyticsStore::new(rusqlite::Connection::open(dir.path().join("a.sqlite3")).unwrap()).unwrap());
        let mut salts = Salts::default();

        let (a, previous) = salts.user_ids(&store, 1000, &["ua", "ip"]).unwrap();
        assert!(previous.is_none());
        assert_eq!(salts.user_ids(&store, 2000, &["ua", "ip"]).unwrap().0, a);
        assert!(a < 1 << 48);

        let (b, previous) = salts.user_ids(&store, DAY + 5, &["ua", "ip"]).unwrap();
        assert_ne!(a, b);
        assert_eq!(previous, Some(a));

        // A restart picks the same salts back up.
        let mut again = Salts::default();
        assert_eq!(again.user_ids(&store, DAY + 10, &["ua", "ip"]).unwrap(), (b, Some(a)));
        assert_eq!(store.lock().salts().unwrap().len(), 2);
    }
}
