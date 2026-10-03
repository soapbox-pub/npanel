//! Certificates and ACME accounts, in the database.
//!
//! Each belongs to one CA's directory, so switching between Let's Encrypt
//! and its staging CA never serves one's certificate under the other, and
//! switching back finds the first's still there.
//!
//! A certificate row also records how getting one last went — failures in a
//! row, when, and why — so the backoff a hostname has earned survives a
//! restart. Without that, a server restarting in a loop would ask the CA
//! again every time, and spend the few failed validations an hour Let's
//! Encrypt allows a name.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use serde::Serialize;

/// A certificate and its key, ready to serve.
pub struct Certificate {
    pub key: Arc<CertifiedKey>,
    /// Its validity period, in Unix seconds.
    pub not_before: i64,
    pub not_after: i64,
}

impl Certificate {
    /// A chain, leaf first, and its private key, both PEM.
    pub fn from_pem(chain: &str, key: &str) -> Result<Certificate> {
        let certificates =
            CertificateDer::pem_slice_iter(chain.as_bytes()).collect::<Result<Vec<_>, _>>().context("Its certificates can't be read")?;
        let Some(leaf) = certificates.first() else { bail!("It holds no certificate") };
        let (_, parsed) = x509_parser::parse_x509_certificate(leaf).map_err(|error| anyhow!("Its certificate can't be parsed: {error}"))?;
        let validity = parsed.validity();
        let (not_before, not_after) = (validity.not_before.timestamp(), validity.not_after.timestamp());
        let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).context("Its private key can't be read")?;
        let signer = rustls::crypto::ring::sign::any_supported_type(&key).context("Its private key isn't a supported type")?;
        let key = CertifiedKey::new(certificates, signer);
        key.keys_match().context("Its private key doesn't match its certificate")?;
        Ok(Certificate { key: Arc::new(key), not_before, not_after })
    }
}

/// When a certificate is renewed: with a third of its lifetime left, which is
/// a month for a 90-day certificate and two days for a six-day one.
pub fn renew_at(not_before: i64, not_after: i64) -> i64 {
    not_after - (not_after - not_before).max(0) / 3
}

/// What is known about one name under one CA.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub hostname: String,
    /// The certificate's validity, if one was ever issued.
    pub validity: Option<(i64, i64)>,
    /// Attempts that have failed since the last one that worked.
    pub failures: i64,
    pub attempted_at: Option<i64>,
}

/// Every row under `directory`.
pub fn rows(conn: &Connection, directory: &str) -> rusqlite::Result<Vec<Row>> {
    conn.prepare_cached("SELECT hostname, not_before, not_after, failures, attempted_at FROM certificates WHERE directory = ?")?
        .query_map([directory], |r| {
            let validity = match (r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?) {
                (Some(not_before), Some(not_after)) => Some((not_before, not_after)),
                _ => None,
            };
            Ok(Row { hostname: r.get(0)?, validity, failures: r.get(3)?, attempted_at: r.get(4)? })
        })?
        .collect()
}

/// What [`load`] found.
pub struct Loaded {
    /// By hostname, ready to serve.
    pub certificates: Vec<(String, Certificate)>,
    /// Hostnames whose stored certificate can't be served, and why.
    pub unusable: Vec<(String, String)>,
}

/// The unexpired certificates for `names` under `directory`. One that can't
/// be read is left out and reported, so it is issued again rather than
/// taking the others down.
pub fn load(conn: &Connection, directory: &str, names: &BTreeSet<String>, now: i64) -> rusqlite::Result<Loaded> {
    let stored = conn
        .prepare_cached(
            "SELECT hostname, chain, private_key FROM certificates
             WHERE directory = ? AND chain IS NOT NULL AND not_after > ?",
        )?
        .query_map(params![directory, now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut loaded = Loaded { certificates: Vec::new(), unusable: Vec::new() };
    for (hostname, chain, key) in stored {
        if !names.contains(&hostname) {
            continue;
        }
        match Certificate::from_pem(&chain, &key) {
            Ok(certificate) => loaded.certificates.push((hostname, certificate)),
            Err(error) => loaded.unusable.push((hostname, format!("{error:#}"))),
        }
    }
    Ok(loaded)
}

/// Keep a newly issued certificate, valid from and until the two times in
/// `validity`, and forget the failures before it.
pub fn save(conn: &Connection, directory: &str, hostname: &str, chain: &str, key: &str, validity: (i64, i64), now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO certificates (directory, hostname, chain, private_key, not_before, not_after, issued_at, failures, attempted_at, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?7, NULL)
         ON CONFLICT (directory, hostname) DO UPDATE SET
           chain = ?3, private_key = ?4, not_before = ?5, not_after = ?6, issued_at = ?7, failures = 0, attempted_at = ?7, error = NULL",
        params![directory, hostname, chain, key, validity.0, validity.1, now],
    )?;
    Ok(())
}

/// Note an attempt that failed. Whatever certificate there was is kept: it
/// goes on serving until it expires.
pub fn record_failure(conn: &Connection, directory: &str, hostname: &str, error: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO certificates (directory, hostname, failures, attempted_at, error) VALUES (?1, ?2, 1, ?3, ?4)
         ON CONFLICT (directory, hostname) DO UPDATE SET failures = failures + 1, attempted_at = ?3, error = ?4",
        params![directory, hostname, now, error],
    )?;
    Ok(())
}

/// Forget names nothing wants any more, once there's nothing left worth
/// keeping: a certificate that has expired, and a failure older than
/// `forget_failures_before`. A certificate still valid is kept in case its
/// name comes back, since issuing it again would spend the CA's limits on
/// duplicates.
pub fn forget(conn: &Connection, wanted: &BTreeSet<String>, now: i64, forget_failures_before: i64) -> rusqlite::Result<usize> {
    let stale = conn
        .prepare_cached(
            "SELECT directory, hostname FROM certificates
             WHERE COALESCE(not_after, 0) <= ? AND COALESCE(attempted_at, 0) < ?",
        )?
        .query_map(params![now, forget_failures_before], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut forgotten = 0;
    for (directory, hostname) in stale {
        if !wanted.contains(&hostname) {
            forgotten += conn.execute("DELETE FROM certificates WHERE directory = ? AND hostname = ?", params![directory, hostname])?;
        }
    }
    Ok(forgotten)
}

/// The account kept for `directory`, and the email it was registered with.
pub fn account(conn: &Connection, directory: &str) -> rusqlite::Result<Option<(String, String)>> {
    conn.prepare_cached("SELECT credentials, email FROM acme_accounts WHERE directory = ?")?
        .query_row([directory], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
}

pub fn save_account(conn: &Connection, directory: &str, credentials: &str, email: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO acme_accounts (directory, credentials, email, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (directory) DO UPDATE SET credentials = ?2, email = ?3",
        params![directory, credentials, email, now],
    )?;
    Ok(())
}

pub fn set_account_email(conn: &Connection, directory: &str, email: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE acme_accounts SET email = ? WHERE directory = ?", params![email, directory])?;
    Ok(())
}

/// One name's certificate, as the dashboard shows it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CertificateStatus {
    pub hostname: String,
    /// When the certificate being served expires, if there is one.
    pub expires_at: Option<i64>,
    /// Why the last attempt failed, if it did.
    pub error: Option<String>,
    pub failures: i64,
    pub attempted_at: Option<i64>,
}

fn status_row(r: &rusqlite::Row) -> rusqlite::Result<CertificateStatus> {
    Ok(CertificateStatus { hostname: r.get(0)?, expires_at: r.get(1)?, error: r.get(2)?, failures: r.get(3)?, attempted_at: r.get(4)? })
}

const STATUS_COLUMNS: &str = "hostname, CASE WHEN not_after > ?2 THEN not_after END, error, failures, attempted_at";

/// The certificate for `hostname` under `directory`. `None` when nothing has
/// been tried yet.
pub fn status(conn: &Connection, directory: &str, hostname: &str, now: i64) -> rusqlite::Result<Option<CertificateStatus>> {
    conn.prepare_cached(&format!("SELECT {STATUS_COLUMNS} FROM certificates WHERE directory = ?1 AND hostname = ?3"))?
        .query_row(params![directory, now, hostname], status_row)
        .optional()
}

/// Every name under `directory` whose last attempt failed.
pub fn failing(conn: &Connection, directory: &str, now: i64) -> rusqlite::Result<Vec<CertificateStatus>> {
    conn.prepare_cached(&format!("SELECT {STATUS_COLUMNS} FROM certificates WHERE directory = ?1 AND failures > 0 ORDER BY hostname"))?
        .query_map(params![directory, now], status_row)?
        .collect()
}

/// How many names under `directory` have a certificate that hasn't expired.
pub fn issued(conn: &Connection, directory: &str, now: i64) -> rusqlite::Result<i64> {
    conn.prepare_cached("SELECT COUNT(*) FROM certificates WHERE directory = ? AND not_after > ?")?
        .query_row(params![directory, now], |r| r.get(0))
}

#[cfg(test)]
pub mod testing {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
    use rustls::pki_types::CertificateDer;

    /// A CA, and a certificate from it for `names`: the chain and key as a CA
    /// would hand them over, PEM.
    pub struct Issued {
        pub ca: CertificateDer<'static>,
        pub chain: String,
        pub key: String,
    }

    pub fn issue(names: &[&str]) -> Issued {
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let params = CertificateParams::new(names.iter().map(|name| name.to_string()).collect::<Vec<_>>()).unwrap();
        let key = KeyPair::generate().unwrap();
        let leaf = params.signed_by(&key, &ca).unwrap();
        Issued { ca: ca.as_ref().der().clone(), chain: format!("{}{}", leaf.pem(), ca.as_ref().pem()), key: key.serialize_pem() }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::issue;
    use super::*;

    const LE: &str = "https://acme-v02.api.letsencrypt.org/directory";
    const STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

    #[test]
    fn renews_with_a_third_left() {
        assert_eq!(renew_at(0, 90), 60);
        assert_eq!(renew_at(100, 106), 104);
    }

    #[test]
    fn reads_what_a_ca_issues() {
        let issued = issue(&["example.org"]);
        let certificate = Certificate::from_pem(&issued.chain, &issued.key).unwrap();
        assert_eq!(certificate.key.cert.len(), 2);
        assert!(certificate.not_after > certificate.not_before);

        let other = issue(&["example.org"]);
        let mismatched = Certificate::from_pem(&issued.chain, &other.key).err().unwrap();
        assert!(mismatched.to_string().contains("doesn't match"));
        assert!(Certificate::from_pem("", &issued.key).is_err());
    }

    #[test]
    fn keeps_certificates_per_ca_and_failures_across_attempts() {
        let t = crate::db::testing::temp_db();
        let issued = issue(&["example.org"]);
        let certificate = Certificate::from_pem(&issued.chain, &issued.key).unwrap();
        let names: BTreeSet<String> = ["example.org".to_string()].into();
        t.db.with(|conn| {
            record_failure(conn, LE, "example.org", "no", 10)?;
            record_failure(conn, LE, "example.org", "still no", 20)?;
            assert_eq!(rows(conn, LE)?[0].failures, 2);
            assert_eq!(failing(conn, LE, 0)?[0].error.as_deref(), Some("still no"));

            save(conn, LE, "example.org", &issued.chain, &issued.key, (certificate.not_before, certificate.not_after), 30)?;
            let row = &rows(conn, LE)?[0];
            assert_eq!((row.failures, row.validity), (0, Some((certificate.not_before, certificate.not_after))));
            assert!(failing(conn, LE, 0)?.is_empty());
            assert_eq!(load(conn, LE, &names, 0)?.certificates.len(), 1);
            assert!(load(conn, STAGING, &names, 0)?.certificates.is_empty());
            assert!(load(conn, LE, &BTreeSet::new(), 0)?.certificates.is_empty());

            // A failed renewal keeps the certificate serving.
            record_failure(conn, LE, "example.org", "renewal failed", 40)?;
            assert_eq!(load(conn, LE, &names, 0)?.certificates.len(), 1);
            assert_eq!(status(conn, LE, "example.org", 0)?.unwrap().expires_at, Some(certificate.not_after));
            assert_eq!(super::issued(conn, LE, 0)?, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn forgets_only_what_is_unwanted_and_spent() {
        let t = crate::db::testing::temp_db();
        let issued = issue(&["kept.example"]);
        let certificate = Certificate::from_pem(&issued.chain, &issued.key).unwrap();
        t.db.with(|conn| {
            save(conn, LE, "kept.example", &issued.chain, &issued.key, (certificate.not_before, certificate.not_after), 0)?;
            record_failure(conn, LE, "old.example", "no", 10)?;
            record_failure(conn, LE, "recent.example", "no", 1000)?;
            record_failure(conn, LE, "wanted.example", "no", 10)?;
            let wanted: BTreeSet<String> = ["wanted.example".to_string()].into();
            assert_eq!(forget(conn, &wanted, 100, 500)?, 1);
            let left: Vec<String> = rows(conn, LE)?.into_iter().map(|row| row.hostname).collect();
            assert_eq!(left.len(), 3);
            assert!(!left.contains(&"old.example".to_string()));
            Ok(())
        })
        .unwrap();
    }
}
