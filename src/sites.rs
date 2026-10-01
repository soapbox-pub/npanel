//! Queries over stored versions, and what serving asks of them.

use std::collections::HashMap;

use indexmap::IndexMap;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::blobs::Variants;
use crate::hash::BlobHash;
use crate::db::{DeployStatus, Host, SiteEvent, HOST_COLUMNS, HOST_COLUMN_COUNT, SITE_EVENT_COLUMNS};
use crate::hostname::is_onion_address;
use crate::log::Fields;
use crate::manifest::{join_root, manifest_address, manifest_paths};
use crate::nostr::Event;
use crate::nsite_address::NsiteAddress;
use crate::util::{decode_path, extname};

/// Stable key for a site, used to group hosts pointing at the same nsite.
pub fn site_key(address: &NsiteAddress) -> String {
    format!("{}:{}:{}", address.kind, address.pubkey, address.identifier)
}

/// Every distinct site referenced by an enabled host.
pub fn list_configured_sites(conn: &Connection) -> rusqlite::Result<Vec<NsiteAddress>> {
    conn.prepare_cached("SELECT DISTINCT pubkey, kind, identifier FROM hosts WHERE enabled = 1")?
        .query_map([], |row| Ok(NsiteAddress { pubkey: row.get(0)?, kind: row.get(1)?, identifier: row.get(2)? }))?
        .collect()
}

/// A host, with the onion address it also answers at if Tor has given it one.
#[derive(Clone, Debug)]
pub struct ServedHost {
    pub host: Host,
    pub onion: Option<String>,
}

/// Look up a host by any name it answers to: its hostname, or its onion
/// address. Which column to match is decided by the shape of the name, which
/// is unambiguous: `parse_hostname` refuses to store a `.onion` as a hostname.
pub fn find_host(conn: &Connection, hostname: &str) -> rusqlite::Result<Option<ServedHost>> {
    let name = hostname.to_lowercase();
    let column = if is_onion_address(&name) { "onion_services.address" } else { "hosts.hostname" };

    conn.prepare_cached(&format!(
        "SELECT {HOST_COLUMNS}, onion_services.address FROM hosts
         LEFT JOIN onion_services ON onion_services.host_id = hosts.id
         WHERE {column} = ?"
    ))?
    .query_row([name], |row| Ok(ServedHost { host: Host::from_row(row)?, onion: row.get(HOST_COLUMN_COUNT)? }))
    .optional()
}

pub fn load_host(conn: &Connection, hostname: &str) -> rusqlite::Result<Option<Host>> {
    conn.prepare_cached(&format!("SELECT {HOST_COLUMNS} FROM hosts WHERE hostname = ?"))?
        .query_row([hostname.to_lowercase()], Host::from_row)
        .optional()
}

pub fn host_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<Host>> {
    conn.prepare_cached(&format!("SELECT {HOST_COLUMNS} FROM hosts WHERE id = ?"))?.query_row([id], Host::from_row).optional()
}

pub fn site_event(conn: &Connection, event_id: &str) -> rusqlite::Result<Option<SiteEvent>> {
    conn.prepare_cached(&format!("SELECT {SITE_EVENT_COLUMNS} FROM site_events WHERE event_id = ?"))?
        .query_row([event_id], SiteEvent::from_row)
        .optional()
}

/// The version a host currently serves: its pinned event if it has one,
/// otherwise the newest version whose blobs are all downloaded.
///
/// A deploy that is still fetching blobs, or that failed, is never returned —
/// the previous working version keeps serving until the new one is complete.
pub fn active_event(conn: &Connection, host: &Host) -> rusqlite::Result<Option<SiteEvent>> {
    if let Some(pinned) = &host.pinned_event_id {
        return conn
            .prepare_cached(&format!("SELECT {SITE_EVENT_COLUMNS} FROM site_events WHERE event_id = ? AND status = 'ready'"))?
            .query_row([pinned], SiteEvent::from_row)
            .optional();
    }
    newest_ready_event(conn, &host.address())
}

/// The newest fully-downloaded version of a site.
///
/// `status = 'ready'` is a literal, not a bound parameter:
/// `site_events_ready_idx` is partial on it, and SQLite only uses a partial
/// index when the query's text provably implies its predicate.
pub fn newest_ready_event(conn: &Connection, address: &NsiteAddress) -> rusqlite::Result<Option<SiteEvent>> {
    conn.prepare_cached(&format!(
        "SELECT {SITE_EVENT_COLUMNS} FROM site_events
         WHERE pubkey = ? AND kind = ? AND identifier = ? AND status = 'ready'
         ORDER BY created_at DESC LIMIT 1"
    ))?
    .query_row(params![address.pubkey, address.kind, address.identifier], SiteEvent::from_row)
    .optional()
}

/// The newest version of a site in any state.
pub fn newest_event(conn: &Connection, address: &NsiteAddress) -> rusqlite::Result<Option<SiteEvent>> {
    conn.prepare_cached(&format!(
        "SELECT {SITE_EVENT_COLUMNS} FROM site_events
         WHERE pubkey = ? AND kind = ? AND identifier = ?
         ORDER BY created_at DESC LIMIT 1"
    ))?
    .query_row(params![address.pubkey, address.kind, address.identifier], SiteEvent::from_row)
    .optional()
}

/// How many versions a merged host's build reaches across. Deep enough that a
/// site deploying daily keeps two months of superseded asset URLs answering.
pub const MAX_MERGED_VERSIONS: i64 = 60;

/// The versions a host's build merges, newest first.
///
/// One entry — the version being served — unless the host merges
/// deployments, which stacks every earlier ready version underneath it. The
/// order is the precedence: a path is served from the newest version that has
/// it. Nothing newer than the served version is included, so a pinned host
/// merges up to its pin, and only ready versions: merging must not undo the
/// invariant that serving never reaches a blob that may not be on disk.
pub fn version_chain(conn: &Connection, host: &Host, active: &SiteEvent) -> rusqlite::Result<Vec<String>> {
    let mut chain = vec![active.event_id.clone()];
    if !host.merge_deployments {
        return Ok(chain);
    }

    let older = conn
        .prepare_cached(
            "SELECT event_id FROM site_events
             WHERE pubkey = ? AND kind = ? AND identifier = ? AND status = 'ready'
               AND created_at <= ? AND event_id != ?
             ORDER BY created_at DESC, event_id DESC LIMIT ?",
        )?
        .query_map(
            params![active.pubkey, active.kind, active.identifier, active.created_at, active.event_id, MAX_MERGED_VERSIONS - 1],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    chain.extend(older);
    Ok(chain)
}

/// One path of a build resolved: the file, and what to serve it as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path: String,
    pub sha256: BlobHash,
    /// A file the site really has (200), or its not-found page (404).
    pub status: u16,
    /// The blob's sizes, when a deploy has recorded them. `None` for rows that
    /// predate the sizes being recorded; the caller stats the disk instead.
    pub variants: Option<Variants>,
}

/// Resolve a request path within a host's build: exact match, then — for an
/// extensionless path — a `.html` sibling and then `index.html` beneath it,
/// and finally the site's own `/404.html`.
///
/// The `.html` step is what a multi-page generator needs: VitePress with
/// `cleanUrls` emits `self-hosting.html` and links to `/self-hosting`. It is
/// tried ahead of `index.html` because that is the order `try_files {path}
/// {path}.html {path}/index.html` established.
///
/// `spa` turns the last step into a client-side router's entry point: a
/// document request gets the site's shell under a `200` — `/404.html` first,
/// since NIP-5A names it as the fallback path, then `/index.html`. Asset
/// requests are untouched: a script that answers `200` with a page of HTML
/// fails somewhere harder to read.
pub fn resolve_site_path(
    conn: &Connection,
    build_id: i64,
    request_path: &str,
    spa: bool,
) -> rusqlite::Result<Option<ResolvedFile>> {
    let normalized = normalize_site_path(request_path);
    let extension = extname(&normalized);
    let extensionless = extension.is_empty();
    let html_path = (extensionless && normalized != "/").then(|| format!("{normalized}.html"));
    let index_path = extensionless.then(|| join_root(&format!("{normalized}/index.html")));
    let shell = spa && matches!(extension.to_lowercase().as_str(), "" | ".html" | ".htm");

    let mut candidates: Vec<&str> = vec![&normalized];
    candidates.extend(html_path.as_deref());
    candidates.extend(index_path.as_deref());
    candidates.push("/404.html");
    if shell {
        candidates.push("/index.html");
    }

    // The blob's sizes ride along on the lookup the request makes anyway, so
    // serving a recorded blob touches the filesystem only to send it.
    let placeholders = vec!["?"; candidates.len()].join(", ");
    let sql = format!(
        "SELECT host_build_files.path, host_build_files.sha256, blobs.size, blobs.br_size, blobs.gzip_size
         FROM host_build_files LEFT JOIN blobs ON blobs.sha256 = host_build_files.sha256
         WHERE host_build_files.build_id = ? AND host_build_files.path IN ({placeholders})"
    );
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&build_id];
    values.extend(candidates.iter().map(|c| c as &dyn rusqlite::ToSql));

    let found: HashMap<String, (BlobHash, Option<Variants>)> = conn
        .prepare_cached(&sql)?
        .query_map(values.as_slice(), |row| {
            let size: Option<i64> = row.get(2)?;
            let br: Option<i64> = row.get(3)?;
            let gzip: Option<i64> = row.get(4)?;
            Ok((row.get::<_, String>(0)?, (row.get(1)?, recorded_variants(size, br, gzip))))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let resolved = |path: &str, status: u16| {
        found.get(path).map(|(sha256, variants)| ResolvedFile {
            path: path.to_string(),
            sha256: *sha256,
            status,
            variants: *variants,
        })
    };

    if let Some(file) = resolved(&normalized, 200) {
        return Ok(Some(file));
    }
    if let Some(file) = html_path.as_deref().and_then(|p| resolved(p, 200)) {
        return Ok(Some(file));
    }
    if let Some(file) = index_path.as_deref().and_then(|p| resolved(p, 200)) {
        return Ok(Some(file));
    }
    if let Some(file) = resolved("/404.html", if shell { 200 } else { 404 }) {
        return Ok(Some(file));
    }
    if shell {
        return Ok(resolved("/index.html", 200));
    }
    Ok(None)
}

/// The sizes a deploy recorded for a blob, or `None` when it hasn't. `NULL`
/// anywhere means the row predates the sizes being recorded; -1 is a recorded
/// answer: no such copy is kept.
fn recorded_variants(size: Option<i64>, br: Option<i64>, gzip: Option<i64>) -> Option<Variants> {
    let (size, br, gzip) = (size?, br?, gzip?);
    Some(Variants {
        identity: size as u64,
        br: (br >= 0).then_some(br as u64),
        gzip: (gzip >= 0).then_some(gzip as u64),
    })
}

/// One row per path: the one from the newest version of the chain that has
/// it. A single-version chain reduces to that version's own map.
pub fn newest_by_path<T>(rows: Vec<T>, event_ids: &[String], key: impl Fn(&T) -> (&str, &str)) -> indexmap::IndexMap<String, T> {
    let rank: HashMap<&str, usize> = event_ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
    let rank_of = |event_id: &str| rank.get(event_id).copied().unwrap_or(usize::MAX);

    let mut best: indexmap::IndexMap<String, (usize, T)> = indexmap::IndexMap::new();
    for row in rows {
        let (path, event_id) = key(&row);
        let row_rank = rank_of(event_id);
        let path = path.to_string();
        match best.get(&path) {
            Some((previous, _)) if *previous <= row_rank => {}
            _ => {
                best.insert(path, (row_rank, row));
            }
        }
    }

    best.into_iter().map(|(path, (_, row))| (path, row)).collect()
}

/// Record a manifest event and its file list. Returns false when the event
/// was already stored, so callers can skip redeploying something already seen.
///
/// A list longer than `max_files` is not expanded: the version is recorded,
/// so its deploy can say why it was refused, but a row per file of a list
/// nobody will deploy is the author's to make the gateway store, as often as
/// they publish. [`expand_manifest_files`] fills it in if the limit is raised.
pub fn store_manifest(conn: &mut Connection, event: &Event, max_files: i64) -> rusqlite::Result<bool> {
    let existing: Option<String> = conn
        .prepare_cached("SELECT event_id FROM site_events WHERE event_id = ?")?
        .query_row([&event.id], |row| row.get(0))
        .optional()?;
    if existing.is_some() {
        return Ok(false);
    }

    let address = manifest_address(event);
    let paths = manifest_paths(event);
    let now = crate::util::now();
    let json = serde_json::to_string(event).unwrap_or_default();

    let tx = conn.transaction()?;
    tx.execute(
        &format!("INSERT INTO site_events ({SITE_EVENT_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, NULL)"),
        params![event.id, address.pubkey, address.kind, address.identifier, event.created_at, json, DeployStatus::Pending, paths.len() as i64, now],
    )?;
    if paths.len() as i64 <= max_files {
        insert_files(&tx, &event.id, &paths)?;
    }
    tx.commit()?;

    Ok(true)
}

/// Record a stored version's file list, if [`store_manifest`] left it out.
pub fn expand_manifest_files(conn: &mut Connection, event: &Event) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let present: bool = tx.prepare_cached("SELECT EXISTS (SELECT 1 FROM site_files WHERE event_id = ?)")?.query_row([&event.id], |r| r.get(0))?;
    if !present {
        insert_files(&tx, &event.id, &manifest_paths(event))?;
    }
    tx.commit()
}

fn insert_files(conn: &Connection, event_id: &str, paths: &IndexMap<String, BlobHash>) -> rusqlite::Result<()> {
    let mut insert = conn.prepare_cached("INSERT INTO site_files (event_id, path, sha256) VALUES (?, ?, ?)")?;
    for (path, sha256) in paths {
        insert.execute(params![event_id, path, sha256])?;
    }
    Ok(())
}

/// A request path as resolution sees it: percent-decoded and rooted, so it
/// can be compared against the paths `site_files` stores.
pub fn normalize_site_path(request_path: &str) -> String {
    join_root(&decode_path(request_path))
}

/// The `blobs` row for a stored blob: its size, and that something still
/// wants it.
pub fn record_blob(conn: &Connection, sha256: BlobHash, size: u64) -> rusqlite::Result<()> {
    let now = crate::util::now();
    conn.prepare_cached(
        "INSERT INTO blobs (sha256, size, seen_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (sha256) DO UPDATE SET seen_at = ?3, size = ?2",
    )?
    .execute(params![sha256, size as i64, now])?;
    Ok(())
}

/// Record which compressed copies a blob has, so serving never has to look.
/// Recorded for every blob, not only the compressible ones: a JPEG's row
/// saying -1 is what lets a request for it skip the stat too.
pub fn record_variant_sizes(conn: &Connection, sha256: BlobHash, variants: Variants) -> rusqlite::Result<()> {
    conn.prepare_cached("UPDATE blobs SET br_size = ?, gzip_size = ? WHERE sha256 = ?")?.execute(params![
        variants.br.map_or(-1, |s| s as i64),
        variants.gzip.map_or(-1, |s| s as i64),
        sha256
    ])?;
    Ok(())
}

/// Deploy log entries kept per version.
const DEPLOY_LOG_LIMIT: i64 = 500;

/// Append to the log the dashboard shows for a version.
pub fn write_deploy_log(conn: &Connection, event_id: &str, level: &str, msg: &str, fields: Fields) -> rusqlite::Result<()> {
    let object: serde_json::Map<String, Value> = fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let fields = (!object.is_empty()).then(|| Value::Object(object).to_string());
    conn.prepare_cached(
        "INSERT INTO deploy_log (event_id, level, msg, fields, created_at) VALUES (?, ?, ?, ?, ?)",
    )?
    .execute(params![event_id, level, msg, fields, crate::util::now()])?;
    Ok(())
}

/// Drop all but the newest entries for a version.
pub fn prune_deploy_log(conn: &Connection, event_id: &str) -> rusqlite::Result<()> {
    let cutoff: Option<i64> = conn
        .prepare_cached("SELECT id FROM deploy_log WHERE event_id = ? ORDER BY id DESC LIMIT 1 OFFSET ?")?
        .query_row(params![event_id, DEPLOY_LOG_LIMIT], |row| row.get(0))
        .optional()?;

    if let Some(cutoff) = cutoff {
        conn.prepare_cached("DELETE FROM deploy_log WHERE event_id = ? AND id <= ?")?.execute(params![event_id, cutoff])?;
    }
    Ok(())
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// Insert a host, returning its id.
    pub fn insert_host(conn: &Connection, hostname: &str, address: &NsiteAddress) -> i64 {
        conn.execute(
            "INSERT INTO hosts (hostname, pubkey, kind, identifier, pinned_event_id, enabled, created_by, created_at, updated_at)
             VALUES (?, ?, ?, ?, NULL, 1, ?, 1, 1)",
            params![hostname, address.pubkey, address.kind, address.identifier, "b".repeat(64)],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Insert a ready version with the given files.
    pub fn insert_version(conn: &Connection, event_id: &str, address: &NsiteAddress, created_at: i64, files: &[(&str, &str)]) {
        conn.execute(
            &format!("INSERT INTO site_events ({SITE_EVENT_COLUMNS}) VALUES (?, ?, ?, ?, ?, '{{}}', 'ready', ?, ?, NULL, ?, ?)"),
            params![event_id, address.pubkey, address.kind, address.identifier, created_at, files.len() as i64, files.len() as i64, created_at, created_at],
        )
        .unwrap();
        for (path, sha) in files {
            conn.execute("INSERT INTO site_files (event_id, path, sha256) VALUES (?, ?, ?)", params![event_id, path, sha]).unwrap();
        }
    }

    /// Insert a ready build of a host with the given files.
    pub fn insert_build(conn: &Connection, host_id: i64, event_id: &str, files: &[(&str, &str)]) -> i64 {
        conn.execute(
            "INSERT INTO host_builds (host_id, kind, origin, event_id, fingerprint, status, error, last_modified, created_at, built_at)
             VALUES (?, 'clearnet', 'https://x', ?, 'f', 'ready', NULL, 1, 1, 1)",
            params![host_id, event_id],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        for (path, sha) in files {
            conn.execute(
                "INSERT INTO host_build_files (build_id, path, sha256, event_id, source_sha256) VALUES (?, ?, ?, ?, NULL)",
                params![id, path, sha, event_id],
            )
            .unwrap();
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn address() -> NsiteAddress {
        NsiteAddress { pubkey: "a".repeat(64), kind: 15128, identifier: String::new() }
    }

    #[test]
    fn resolves_paths() {
        let t = crate::db::testing::temp_db();
        t.db.with(|conn| {
            let host = insert_host(conn, "docs.example.com", &address());
            let sha = |c: char| c.to_string().repeat(64);
            let build = insert_build(
                conn,
                host,
                "e",
                &[
                    ("/index.html", &sha('1')),
                    ("/about.html", &sha('2')),
                    ("/guide/index.html", &sha('3')),
                    ("/404.html", &sha('4')),
                    ("/app.js", &sha('5')),
                ],
            );

            let at = |path: &str, spa: bool| resolve_site_path(conn, build, path, spa).unwrap().map(|f| (f.path, f.status));
            assert_eq!(at("/", false), Some(("/index.html".into(), 200)));
            assert_eq!(at("/about", false), Some(("/about.html".into(), 200)));
            assert_eq!(at("/guide", false), Some(("/guide/index.html".into(), 200)));
            assert_eq!(at("/guide/", false), Some(("/guide/index.html".into(), 200)));
            assert_eq!(at("/missing", false), Some(("/404.html".into(), 404)));
            assert_eq!(at("/missing", true), Some(("/404.html".into(), 200)));
            assert_eq!(at("/missing.js", true), Some(("/404.html".into(), 404)));
            assert_eq!(at("/a%62out.html", false), Some(("/about.html".into(), 200)));
            assert_eq!(at("/../app.js", false), Some(("/app.js".into(), 200)));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn merges_newest_first() {
        let rows = vec![("/a", "old"), ("/a", "new"), ("/b", "old")];
        let chain = vec!["new".to_string(), "old".to_string()];
        let merged = newest_by_path(rows, &chain, |row| (row.0, row.1));
        assert_eq!(merged["/a"].1, "new");
        assert_eq!(merged["/b"].1, "old");
    }

    #[test]
    fn stores_manifests_once() {
        let t = crate::db::testing::temp_db();
        let event = Event {
            id: "c".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 5,
            kind: 15128,
            tags: vec![vec!["path".into(), "/index.html".into(), "d".repeat(64)]],
            content: String::new(),
            sig: "0".repeat(128),
        };
        t.db.with(|conn| {
            assert!(store_manifest(conn, &event, 100)?);
            assert!(!store_manifest(conn, &event, 100)?);
            let newest = newest_event(conn, &address())?.unwrap();
            assert_eq!(newest.files_total, 1);
            assert!(newest_ready_event(conn, &address())?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn leaves_long_file_lists_until_wanted() {
        let t = crate::db::testing::temp_db();
        let event = Event {
            id: "e".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 5,
            kind: 15128,
            tags: vec![vec!["path".into(), "/a".into(), "d".repeat(64)], vec!["path".into(), "/b".into(), "f".repeat(64)]],
            content: String::new(),
            sig: "0".repeat(128),
        };
        let count = |conn: &Connection| conn.query_row("SELECT COUNT(*) FROM site_files", [], |r| r.get::<_, i64>(0));
        t.db.with(|conn| {
            assert!(store_manifest(conn, &event, 1)?);
            assert_eq!(newest_event(conn, &address())?.unwrap().files_total, 2);
            assert_eq!(count(conn)?, 0);
            expand_manifest_files(conn, &event)?;
            expand_manifest_files(conn, &event)?;
            assert_eq!(count(conn)?, 2);
            Ok(())
        })
        .unwrap();
    }
}
