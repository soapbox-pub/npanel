//! Which hostnames may have a certificate.
//!
//! One rule, asked two ways: name by name by Caddy's `/tls-check`, and all at
//! once by npanel's own ACME client. Both are here so they can't drift: a name
//! the proxy would be allowed a certificate for is exactly a name npanel
//! would get one for itself.
//!
//! The dashboard hostname always qualifies, and is the caller's to add: it is
//! configuration, not a row.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};

use crate::hostname::www_apex;

/// Whether npanel answers for `domain` as a site: an enabled host or tunnel,
/// or the `www` of a host that redirects it. A redirect still needs a
/// certificate, since it is an HTTPS response like any other.
pub fn qualifies(conn: &Connection, domain: &str) -> rusqlite::Result<bool> {
    let configured: Option<i64> = conn
        .prepare_cached("SELECT id FROM hosts WHERE hostname = ?1 AND enabled = 1 UNION ALL SELECT id FROM tunnels WHERE hostname = ?1 AND enabled = 1")?
        .query_row([domain], |r| r.get(0))
        .optional()?;
    if configured.is_some() {
        return Ok(true);
    }
    let Some(apex) = www_apex(domain) else {
        return Ok(false);
    };
    let redirects: Option<i64> = conn
        .prepare_cached("SELECT id FROM hosts WHERE hostname = ? AND enabled = 1 AND redirect_www = 1")?
        .query_row([apex], |r| r.get(0))
        .optional()?;
    Ok(redirects.is_some())
}

/// Every name [`qualifies`] says yes to.
pub fn all(conn: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    // A tunnel's hostname is its only one: nothing redirects to it.
    let hosts = conn
        .prepare_cached("SELECT hostname, redirect_www FROM hosts WHERE enabled = 1 UNION ALL SELECT hostname, 0 FROM tunnels WHERE enabled = 1")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(hosts.into_iter().flat_map(|(hostname, redirect_www)| of_host(&hostname, true, redirect_www)).collect())
}

/// The names one host has certificates for.
pub fn of_host(hostname: &str, enabled: bool, redirect_www: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    let mut names = vec![hostname.to_string()];
    if redirect_www {
        // Asked the way `qualifies` asks it, so the two can't disagree about
        // what a root domain is.
        let www = format!("www.{hostname}");
        if www_apex(&www) == Some(hostname) {
            names.push(www);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ways_agree() {
        let t = crate::db::testing::temp_db();
        t.db.with(|conn| {
            for (hostname, enabled, redirect_www) in [
                ("example.org", true, true),
                ("example.net", true, false),
                ("docs.example.org", true, true),
                ("off.example", false, true),
                ("example.co.uk", true, true),
                ("www.example.com", true, false),
            ] {
                conn.execute(
                    "INSERT INTO hosts (hostname, pubkey, kind, enabled, redirect_www, created_by, created_at, updated_at)
                     VALUES (?, 'p', 15128, ?, ?, 'p', 0, 0)",
                    rusqlite::params![hostname, enabled, redirect_www],
                )?;
            }
            for (hostname, pubkey, enabled) in [("dev.example.org", "a", true), ("off.example.org", "b", false)] {
                conn.execute(
                    "INSERT INTO tunnels (hostname, pubkey, name, target, enabled, created_by, created_at, updated_at)
                     VALUES (?, ?, 'laptop', 'http://localhost:8080', ?, 'p', 0, 0)",
                    rusqlite::params![hostname, pubkey, enabled],
                )?;
            }
            Ok(())
        })
        .unwrap();

        let names = t.db.with(|conn| Ok(all(conn)?)).unwrap();
        let expected = [
            "dev.example.org",
            "docs.example.org",
            "example.co.uk",
            "example.net",
            "example.org",
            "www.example.co.uk",
            "www.example.com",
            "www.example.org",
        ];
        assert_eq!(names.iter().map(String::as_str).collect::<Vec<_>>(), expected);

        let candidates = [
            "example.org",
            "www.example.org",
            "www.example.net",
            "docs.example.org",
            "www.docs.example.org",
            "dev.example.org",
            "www.dev.example.org",
            "off.example.org",
            "off.example",
            "www.off.example",
            "example.co.uk",
            "www.example.co.uk",
            "www.example.com",
            "example.com",
            "unknown.example",
        ];
        for name in candidates {
            let one = t.db.with(|conn| Ok(qualifies(conn, name)?)).unwrap();
            assert_eq!(one, names.contains(name), "{name}");
        }
    }
}
