//! The maintenance commands: `make-admin`, `prune-blobs`, `compress-blobs`.

use std::collections::HashSet;

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

use crate::hash::BlobHash;
use crate::blobs::BlobStore;
use crate::config::Config;
use crate::db::Db;
use crate::identifiers::parse_pubkey;
use crate::log::Logger;
use crate::mime::is_compressible;
use crate::nostr::nip19::npub_encode;
use crate::sites::{active_event, record_variant_sizes};
use crate::util::format_bytes;

/// Bootstrap the first admin, or promote someone from the command line. There
/// is no other way to get the first account in: the dashboard's user
/// management is admin-only.
pub fn make_admin(config: &Config, log: &Logger, input: Option<&str>) -> Result<()> {
    let Some(input) = input else {
        bail!("Usage: npanel make-admin <npub or hex pubkey>");
    };
    let pubkey = parse_pubkey(input).map_err(anyhow::Error::msg)?;
    let npub = npub_encode(&pubkey);

    let db = Db::open(&config.database_path(), Some(log))?;
    db.with(|conn| {
        let now = crate::util::now();
        let existing: Option<String> = conn.query_row("SELECT role FROM users WHERE pubkey = ?", [&pubkey], |r| r.get(0)).optional()?;
        match existing.as_deref() {
            Some("admin") => println!("{npub} is already an admin."),
            Some(role) => {
                conn.execute("UPDATE users SET role = 'admin' WHERE pubkey = ?", [&pubkey])?;
                conn.execute(
                    "INSERT INTO audit_log (actor_pubkey, action, target, old_value, new_value, created_at) VALUES (?, 'user.update', ?, ?, ?, ?)",
                    params![pubkey, pubkey, serde_json::json!({ "role": role }).to_string(), r#"{"role":"admin"}"#, now],
                )?;
                println!("Promoted {npub} to admin.");
            }
            None => {
                conn.execute("INSERT INTO users (pubkey, role, added_by, created_at) VALUES (?, 'admin', NULL, ?)", params![pubkey, now])?;
                conn.execute(
                    "INSERT INTO audit_log (actor_pubkey, action, target, old_value, new_value, created_at) VALUES (?, 'user.create', ?, NULL, ?, ?)",
                    params![pubkey, pubkey, r#"{"role":"admin","via":"make-admin"}"#, now],
                )?;
                println!("Added {npub} as an admin.");
            }
        }
        Ok(())
    })
}

/// How long a blob nothing references yet is left alone. A build stores the
/// files it generates before it writes the rows naming them.
const GRACE_SECONDS: i64 = 60 * 60;

/// Reclaim disk space. With `--keep N`, old versions of each site are
/// forgotten first — never the version a host is serving, and never a pinned
/// one. Then every blob no remaining version or build references is deleted.
pub fn prune_blobs(config: &Config, log: &Logger, args: &[String]) -> Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let keep = match args.iter().position(|a| a == "--keep") {
        None => None,
        Some(index) => match args.get(index + 1).and_then(|v| v.parse::<usize>().ok()).filter(|n| *n >= 1) {
            Some(n) => Some(n),
            None => bail!("--keep requires a positive integer."),
        },
    };

    let db = Db::open(&config.database_path(), Some(log))?;
    let blobs = BlobStore::new(&config.blobs_dir());

    db.with(|conn| {
        if let Some(keep) = keep {
            let hosts: Vec<crate::db::Host> =
                conn.prepare(&format!("SELECT {} FROM hosts", crate::db::HOST_COLUMNS))?.query_map([], crate::db::Host::from_row)?.collect::<rusqlite::Result<_>>()?;
            let mut protected: HashSet<String> = HashSet::new();
            for host in &hosts {
                if let Some(pinned) = &host.pinned_event_id {
                    protected.insert(pinned.clone());
                }
                if let Some(active) = active_event(conn, host)? {
                    protected.insert(active.event_id);
                }
            }
            // A build serves on without the versions it was made from, but the
            // dashboard describes the site by the version it serves.
            for event_id in conn.prepare("SELECT DISTINCT event_id FROM host_builds")?.query_map([], |r| r.get::<_, String>(0))? {
                protected.insert(event_id?);
            }

            let sites: Vec<(String, i64, String)> = conn
                .prepare("SELECT DISTINCT pubkey, kind, identifier FROM site_events")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;

            let mut removed = 0;
            for (pubkey, kind, identifier) in sites {
                let versions: Vec<String> = conn
                    .prepare("SELECT event_id FROM site_events WHERE pubkey = ? AND kind = ? AND identifier = ? ORDER BY created_at DESC")?
                    .query_map(params![pubkey, kind, identifier], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for event_id in versions.into_iter().skip(keep).filter(|id| !protected.contains(id)) {
                    if !dry_run {
                        // site_files goes with it: it cascades.
                        conn.execute("DELETE FROM deploy_log WHERE event_id = ?", [&event_id])?;
                        conn.execute("DELETE FROM site_events WHERE event_id = ?", [&event_id])?;
                    }
                    removed += 1;
                }
            }
            println!("{} {removed} old version(s), keeping {keep} per site.", if dry_run { "Would remove" } else { "Removed" });
        }

        // Read as text and parsed, so a row that names no real hash references
        // nothing rather than stopping the prune: only hashes on disk are ever
        // deleted, and those always parse.
        let mut referenced: HashSet<BlobHash> = HashSet::new();
        for sql in ["SELECT DISTINCT sha256 FROM site_files", "SELECT DISTINCT sha256 FROM host_build_files"] {
            for sha in conn.prepare(sql)?.query_map([], |r| r.get::<_, String>(0))? {
                referenced.extend(BlobHash::parse(&sha?));
            }
        }
        for sha in conn.prepare("SELECT sha256 FROM blobs WHERE seen_at > ?")?.query_map([crate::util::now() - GRACE_SECONDS], |r| r.get::<_, String>(0))? {
            referenced.extend(BlobHash::parse(&sha?));
        }

        let (mut deleted, mut bytes) = (0, 0u64);
        for sha256 in blobs.list() {
            if referenced.contains(&sha256) {
                continue;
            }
            // Compressed copies go with the blob, so they count too.
            if let Some(v) = blobs.variants(sha256) {
                bytes += v.identity + v.br.unwrap_or(0) + v.gzip.unwrap_or(0);
            }
            if !dry_run {
                blobs.delete(sha256);
                conn.execute("DELETE FROM blobs WHERE sha256 = ?", [sha256])?;
            }
            deleted += 1;
        }
        println!("{} {deleted} unreferenced blob(s), {}.", if dry_run { "Would delete" } else { "Deleted" }, format_bytes(bytes));
        Ok(())
    })
}

/// Store compressed copies of blobs that predate npanel making them, and
/// record every blob's sidecar sizes so serving skips the disk. Safe to re-run.
pub fn compress_blobs(config: &Config, log: &Logger, args: &[String]) -> Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let db = Db::open(&config.database_path(), Some(log))?;
    let blobs = BlobStore::new(&config.blobs_dir());

    // The same rule a deploy uses: the paths a blob is served under decide.
    let rows: Vec<(BlobHash, String)> = db.with(|conn| {
        let mut rows = Vec::new();
        for sql in ["SELECT sha256, path FROM site_files", "SELECT sha256, path FROM host_build_files"] {
            let found = conn.prepare(sql)?.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<(String, String)>>>()?;
            rows.extend(found.into_iter().filter_map(|(sha, path)| Some((BlobHash::parse(&sha)?, path))));
        }
        Ok(rows)
    })?;

    let mut candidates = HashSet::new();
    let mut referenced = indexmap::IndexSet::new();
    for (sha, path) in rows {
        if is_compressible(&path) {
            candidates.insert(sha);
        }
        referenced.insert(sha);
    }
    let total = candidates.len();

    let queue = parking_lot::Mutex::new(referenced.into_iter().collect::<std::collections::VecDeque<_>>());
    let stats = parking_lot::Mutex::new((0usize, 0u64, 0u64, 0u64));

    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| loop {
                let Some(sha256) = queue.lock().pop_front() else { return };
                let Some(mut variants) = blobs.variants(sha256) else { continue };
                let wanted = candidates.contains(&sha256) && (variants.br.is_none() || variants.gzip.is_none());

                if dry_run {
                    if wanted {
                        stats.lock().0 += 1;
                    }
                    continue;
                }

                if wanted {
                    match blobs.ensure_variants(sha256) {
                        Ok(written) if !written.is_empty() => {
                            variants = blobs.variants(sha256).unwrap_or(variants);
                            let smallest = variants.identity.min(variants.br.unwrap_or(u64::MAX)).min(variants.gzip.unwrap_or(u64::MAX));
                            let added: u64 = written.iter().filter_map(|e| variants.get(*e)).sum();
                            let mut s = stats.lock();
                            *s = (s.0 + 1, s.1 + variants.identity, s.2 + smallest, s.3 + added);
                            println!("{sha256}  {} → {}", format_bytes(variants.identity), format_bytes(smallest));
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("{sha256}  {error}"),
                    }
                }

                // The same record a deploy writes. A no-op for blobs with no row.
                let _ = db.with(|conn| Ok(record_variant_sizes(conn, sha256, variants)?));
            });
        }
    });

    let (compressed, before, after, added) = stats.into_inner();
    if dry_run {
        println!("Would compress {compressed} of {total} compressible blob(s).");
    } else if compressed == 0 {
        println!("Nothing to do: all {total} compressible blob(s) already have their copies.");
    } else {
        println!(
            "Compressed {compressed} of {total} compressible blob(s): {} → {} on the wire, for {} of disk.",
            format_bytes(before),
            format_bytes(after),
            format_bytes(added)
        );
    }
    Ok(())
}
