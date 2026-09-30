//! The rows npanel reads back, and the column lists that select them.

use rusqlite::Row;
use serde::Serialize;

use crate::nsite_address::NsiteAddress;

/// A hostname mapped to a site, and every switch that shapes how it is served.
///
/// Flags are kept as the 0/1 the table stores, since that is also how the
/// audit log records them.
#[derive(Clone, Debug, Serialize)]
pub struct Host {
    pub id: i64,
    /// Lowercased hostname, eg `docs.example.com`.
    pub hostname: String,
    /// Hex pubkey of the nsite author.
    pub pubkey: String,
    /// 15128 (root site) or 35128 (named site).
    pub kind: i64,
    /// `d` tag for named sites, empty string for root sites.
    pub identifier: String,
    /// Event id to serve, or null to always serve the newest ready version.
    pub pinned_event_id: Option<String>,
    pub enabled: i64,
    pub hidden: i64,
    pub redirect_www: i64,
    pub spa: i64,
    pub csp: i64,
    pub seo: i64,
    pub merge_deployments: i64,
    pub analytics: i64,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

pub const HOST_COLUMNS: &str = "hosts.id, hosts.hostname, hosts.pubkey, hosts.kind, hosts.identifier, \
    hosts.pinned_event_id, hosts.enabled, hosts.hidden, hosts.redirect_www, hosts.spa, hosts.csp, hosts.seo, \
    hosts.merge_deployments, hosts.analytics, hosts.created_by, hosts.created_at, hosts.updated_at";

impl Host {
    /// Read a host from a row selected with [`HOST_COLUMNS`], starting at
    /// column `at`.
    pub fn from_row_at(row: &Row, at: usize) -> rusqlite::Result<Host> {
        Ok(Host {
            id: row.get(at)?,
            hostname: row.get(at + 1)?,
            pubkey: row.get(at + 2)?,
            kind: row.get(at + 3)?,
            identifier: row.get(at + 4)?,
            pinned_event_id: row.get(at + 5)?,
            enabled: row.get(at + 6)?,
            hidden: row.get(at + 7)?,
            redirect_www: row.get(at + 8)?,
            spa: row.get(at + 9)?,
            csp: row.get(at + 10)?,
            seo: row.get(at + 11)?,
            merge_deployments: row.get(at + 12)?,
            analytics: row.get(at + 13)?,
            created_by: row.get(at + 14)?,
            created_at: row.get(at + 15)?,
            updated_at: row.get(at + 16)?,
        })
    }

    pub fn from_row(row: &Row) -> rusqlite::Result<Host> {
        Self::from_row_at(row, 0)
    }

    pub fn address(&self) -> NsiteAddress {
        NsiteAddress { pubkey: self.pubkey.clone(), kind: self.kind, identifier: self.identifier.clone() }
    }
}

/// Lifecycle of a single site version. A host only serves versions that
/// reached `ready`.
pub mod status {
    pub const PENDING: &str = "pending";
    pub const READY: &str = "ready";
    pub const FAILED: &str = "failed";
}

/// Every manifest seen, verbatim, with its deploy status.
#[derive(Clone, Debug)]
pub struct SiteEvent {
    pub event_id: String,
    pub pubkey: String,
    pub kind: i64,
    pub identifier: String,
    /// `created_at` of the event itself, used to order versions.
    pub created_at: i64,
    /// Raw signed event JSON.
    pub event: String,
    pub status: String,
    pub files_total: i64,
    pub files_ready: i64,
    /// Human-readable reason the deploy failed.
    pub error: Option<String>,
    pub fetched_at: i64,
    pub ready_at: Option<i64>,
}

pub const SITE_EVENT_COLUMNS: &str = "event_id, pubkey, kind, identifier, created_at, event, status, \
    files_total, files_ready, error, fetched_at, ready_at";

impl SiteEvent {
    /// When this version was last modified, as far as anyone downstream is
    /// told: its `created_at`, but never later than npanel saw it, since an
    /// author can date a manifest however they please.
    pub fn last_modified(&self) -> i64 {
        self.created_at.min(self.fetched_at).max(0)
    }

    pub fn from_row(row: &Row) -> rusqlite::Result<SiteEvent> {
        Ok(SiteEvent {
            event_id: row.get(0)?,
            pubkey: row.get(1)?,
            kind: row.get(2)?,
            identifier: row.get(3)?,
            created_at: row.get(4)?,
            event: row.get(5)?,
            status: row.get(6)?,
            files_total: row.get(7)?,
            files_ready: row.get(8)?,
            error: row.get(9)?,
            fetched_at: row.get(10)?,
            ready_at: row.get(11)?,
        })
    }

    pub fn address(&self) -> NsiteAddress {
        NsiteAddress { pubkey: self.pubkey.clone(), kind: self.kind, identifier: self.identifier.clone() }
    }
}

/// Which of a host's names a build answers for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildKind {
    Clearnet,
    Onion,
}

impl BuildKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildKind::Clearnet => "clearnet",
            BuildKind::Onion => "onion",
        }
    }
}

/// The finished bundle a host serves: every path it answers, frozen. Only
/// the columns anything reads back; the rest are written for the dashboard's
/// sake and the record's.
#[derive(Clone, Debug)]
pub struct HostBuild {
    pub id: i64,
    /// The version it serves: the head of the chain it was built from.
    pub event_id: String,
    /// A hash of every input the build's output depends on.
    pub fingerprint: String,
    /// `Last-Modified` for its responses, in epoch seconds.
    pub last_modified: i64,
}

pub const HOST_BUILD_COLUMNS: &str = "id, event_id, fingerprint, last_modified";

impl HostBuild {
    pub fn from_row(row: &Row) -> rusqlite::Result<HostBuild> {
        Ok(HostBuild { id: row.get(0)?, event_id: row.get(1)?, fingerprint: row.get(2)?, last_modified: row.get(3)? })
    }
}
