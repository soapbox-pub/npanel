//! The maintenance commands: `make-admin`, `prune-blobs`, `compress-blobs`.

use std::collections::HashSet;
use std::num::NonZeroUsize;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::hash::BlobHash;
use crate::blobs::BlobStore;
use crate::config::Config;
use crate::db::Db;
use crate::identifiers::parse_pubkey;
use crate::log::Logger;
use crate::mime::is_compressible;
use crate::nostr::nip19::npub_encode;
use crate::prune::{forget_versions, sweep_now};
use crate::settings::Settings;
use crate::sites::record_variant_sizes;
use crate::util::format_bytes;

/// Bootstrap the first admin, or promote someone from the command line. There
/// is no other way to get the first account in: the dashboard's user
/// management is admin-only.
pub fn make_admin(config: &Config, log: &Logger, input: &str) -> Result<()> {
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

/// Reclaim disk space now, as the server does once a day. Versions of sites
/// no host points at are forgotten, and so is everything beyond each site's
/// newest N ready versions — N from `--keep`, or else the `keep_versions`
/// setting. Then every blob no remaining version or build references is
/// deleted.
pub fn prune_blobs(config: &Config, log: &Logger, keep: Option<NonZeroUsize>, dry_run: bool) -> Result<()> {
    let db = Db::open(&config.database_path(), Some(log))?;
    let blobs = BlobStore::new(&config.blobs_dir());
    let keep = match keep {
        Some(keep) => Some(keep.get()),
        None => usize::try_from(Settings::load(&db, None)?.get().keep_versions).ok().filter(|keep| *keep > 0),
    };

    let forgotten = db.with(|conn| forget_versions(conn, keep, dry_run))?;
    let kept = keep.map_or_else(|| "every version of the sites hosts point at".into(), |keep| format!("{keep} ready version(s) per site"));
    println!("{} {} old version(s), keeping {kept}.", if dry_run { "Would remove" } else { "Removed" }, forgotten.versions);
    if !dry_run && forgotten.versions > 0 && !forgotten.rebuild.is_empty() {
        println!("Hosts that merge deployments hold on to those versions' files until the server's next daily prune rebuilds them.");
    }

    // A dry run can't see the blobs the versions it would forget would free,
    // since it forgot nothing.
    let swept = sweep_now(&db, &blobs, dry_run)?;
    println!("{} {} unreferenced blob(s), {}.", if dry_run { "Would delete" } else { "Deleted" }, swept.blobs, format_bytes(swept.bytes));
    Ok(())
}

/// Store compressed copies of blobs that predate npanel making them, and
/// record every blob's sidecar sizes so serving skips the disk. Safe to re-run.
pub fn compress_blobs(config: &Config, log: &Logger, dry_run: bool) -> Result<()> {
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
