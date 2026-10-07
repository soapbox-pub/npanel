//! Stack frames mapped back to the site's original source, through the
//! source maps it deployed.
//!
//! npanel already has every file of every version a site has deployed, so
//! there is nothing to upload: a frame's URL names a file of the version the
//! report says it came from, the file names its source map, and the map is
//! another file of the same version. A site that doesn't publish its maps
//! gets its frames as they were.
//!
//! Done when a report arrives rather than when it is read, because grouping
//! needs it: a minified name changes with every deploy, the original doesn't.

use std::num::NonZeroUsize;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;
use sourcemap::{DecodedMap, SourceView};

use super::envelope::{clean, Context, Exception, Mapped, Report};
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::hash::BlobHash;
use crate::sites::{active_event, host_by_id, newest_by_path, normalize_site_path, site_event, version_chain};

/// Largest script read to map a frame in it.
const MAX_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;
/// Largest source map read.
const MAX_MAP_BYTES: u64 = 64 * 1024 * 1024;
/// Scripts kept parsed, with their maps. A decoded map of a large bundle is
/// tens of megabytes, so only a few.
const MAX_CACHED: usize = 8;
/// Frames mapped per report: the site's own, innermost first.
const MAX_MAPPED_FRAMES: usize = 100;
/// Lines of context either side of a frame.
const CONTEXT_LINES: u32 = 5;
/// Longest line shown as context. Past it, a line is a minified bundle's.
const MAX_CONTEXT_LINE: usize = 300;

/// A script, and the source map it named, if it named one npanel has.
struct Script {
    view: SourceView,
    map: Option<DecodedMap>,
}

pub struct Symbolicator {
    db: Db,
    blobs: BlobStore,
    scripts: Mutex<lru::LruCache<BlobHash, Option<Arc<Script>>>>,
}

impl Symbolicator {
    pub fn new(db: Db, blobs: BlobStore) -> Symbolicator {
        Symbolicator { db, blobs, scripts: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED).unwrap())) }
    }

    /// Mark which of the report's frames are the site's own — served from
    /// `hostname`, the name the report arrived on — and map them. Blocking.
    pub fn symbolicate(&self, report: &mut Report, host_id: i64, hostname: &str) -> anyhow::Result<()> {
        for exception in &mut report.exceptions {
            for frame in &mut exception.frames {
                frame.in_app = site_path(&frame.abs_path, hostname).is_some();
            }
        }
        if !report.exceptions.iter().any(|e| e.frames.iter().any(|f| f.in_app)) {
            return Ok(());
        }

        let release = report.release.clone();
        self.db.with(|conn| {
            let Some(chain) = chain(conn, host_id, release.as_deref())? else { return Ok(()) };
            let files = Files { conn, chain: &chain, blobs: &self.blobs, symbolicator: self };
            let mut budget = MAX_MAPPED_FRAMES;
            for exception in report.exceptions.iter_mut().rev() {
                map_exception(exception, hostname, &files, &mut budget)?;
            }
            Ok(())
        })
    }

    /// The script at `sha256`, parsed, with its map. Parsed outside the lock:
    /// a large map takes a while, and other reports shouldn't wait on it.
    fn script(&self, sha256: BlobHash, path: &str, files: &Files) -> anyhow::Result<Option<Arc<Script>>> {
        if let Some(cached) = self.scripts.lock().get(&sha256) {
            return Ok(cached.clone());
        }
        let loaded = load_script(sha256, path, files)?.map(Arc::new);
        self.scripts.lock().put(sha256, loaded.clone());
        Ok(loaded)
    }
}

/// The versions a report's frames are looked up in: the chain of the
/// version it names, if that is one of this host's site's ready versions,
/// and otherwise of the version the host serves now — a report from a
/// Sentry SDK names a release of its own, or none.
fn chain(conn: &Connection, host_id: i64, release: Option<&str>) -> rusqlite::Result<Option<Vec<String>>> {
    let Some(host) = host_by_id(conn, host_id)? else { return Ok(None) };
    let named = match release {
        Some(release) => site_event(conn, release)?.filter(|e| e.address() == host.address() && e.status == crate::db::DeployStatus::Ready),
        None => None,
    };
    let Some(event) = named.map_or_else(|| active_event(conn, &host), |e| Ok(Some(e)))? else { return Ok(None) };
    Ok(Some(version_chain(conn, &host, &event)?))
}

struct Files<'a> {
    conn: &'a Connection,
    chain: &'a [String],
    blobs: &'a BlobStore,
    symbolicator: &'a Symbolicator,
}

impl Files<'_> {
    /// The blob a site path is in the chain, if any version has it.
    fn find(&self, path: &str) -> rusqlite::Result<Option<BlobHash>> {
        let placeholders = vec!["?"; self.chain.len()].join(", ");
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&path];
        values.extend(self.chain.iter().map(|id| id as &dyn rusqlite::ToSql));
        let rows: Vec<(String, BlobHash, String)> = self
            .conn
            .prepare_cached(&format!("SELECT path, sha256, event_id FROM site_files WHERE path = ? AND event_id IN ({placeholders})"))?
            .query_map(values.as_slice(), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(newest_by_path(rows, self.chain, |row| (row.0.as_str(), row.2.as_str())).into_values().next().map(|row| row.1))
    }
}

/// A frame URL's path within the site, if it is on the site's own origin.
fn site_path(abs_path: &str, hostname: &str) -> Option<String> {
    let url = url::Url::parse(abs_path).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.host_str().is_some_and(|h| h.eq_ignore_ascii_case(hostname)) {
        return None;
    }
    Some(normalize_site_path(url.path()))
}

fn map_exception(exception: &mut Exception, hostname: &str, files: &Files, budget: &mut usize) -> anyhow::Result<()> {
    // Innermost first, so a long stack spends the budget where it matters.
    for index in (0..exception.frames.len()).rev() {
        if *budget == 0 {
            break;
        }
        let frame = &exception.frames[index];
        let (Some(path), Some(lineno)) = (frame.in_app.then(|| site_path(&frame.abs_path, hostname)).flatten(), frame.lineno) else { continue };
        *budget -= 1;
        let Some(sha256) = files.find(&path)? else { continue };
        let Some(script) = files.symbolicator.script(sha256, &path, files)? else { continue };

        let (line, column) = (lineno - 1, frame.colno.unwrap_or(1) - 1);
        // What the frame's caller was calling at the moment: the original
        // name of this frame's function, when nothing better is known.
        let caller_name = index
            .checked_sub(1)
            .map(|caller| &exception.frames[caller])
            .filter(|caller| caller.abs_path == exception.frames[index].abs_path)
            .and_then(|caller| Some((caller.lineno? - 1, caller.colno.unwrap_or(1) - 1)))
            .and_then(|(l, c)| script.map.as_ref()?.lookup_token(l, c)?.get_name().map(str::to_string));

        let frame = &mut exception.frames[index];
        let token = script.map.as_ref().and_then(|map| map.lookup_token(line, column)).filter(|t| t.has_source());
        match token {
            Some(token) => {
                let minified = frame.function.as_deref().and_then(|f| f.rsplit('.').next());
                let original = script.map.as_ref().and_then(|map| map.get_original_function_name(line, column, minified, Some(&script.view)));
                frame.mapped = Some(Mapped {
                    source: clean(&source_name(token.get_source().unwrap_or_default()), 512),
                    function: original.map(str::to_string).or(caller_name).map(|f| clean(&f, 256)),
                    lineno: token.get_src_line() + 1,
                    colno: token.get_src_col() + 1,
                });
                frame.context = token.get_source_view().and_then(|view| context(view, token.get_src_line()));
            }
            None => frame.context = context(&script.view, line),
        }
    }
    Ok(())
}

/// The script, and its source map: the one its `sourceMappingURL` names —
/// inline, or a file of the same versions — or else the file beside it with
/// `.map` appended. Blocking.
fn load_script(sha256: BlobHash, path: &str, files: &Files) -> anyhow::Result<Option<Script>> {
    if files.blobs.size(sha256).is_none_or(|size| size > MAX_SCRIPT_BYTES) {
        return Ok(None);
    }
    let source = files.blobs.read_text(sha256, MAX_SCRIPT_BYTES);
    let reference = source_mapping_url(&source).map(str::to_string);
    let view = SourceView::from_string(source);

    let map = match reference.as_deref() {
        Some(url) if url.starts_with("data:") => sourcemap::SourceMapRef::Ref(url.to_string()).get_embedded_sourcemap().ok().flatten(),
        Some(url) => match resolve(path, url) {
            Some(map_path) => read_map(files, &map_path)?,
            None => None,
        },
        None => read_map(files, &format!("{path}.map"))?,
    };
    Ok(Some(Script { view, map }))
}

fn read_map(files: &Files, path: &str) -> anyhow::Result<Option<DecodedMap>> {
    let Some(sha256) = files.find(path)? else { return Ok(None) };
    if files.blobs.size(sha256).is_none_or(|size| size > MAX_MAP_BYTES) {
        return Ok(None);
    }
    Ok(sourcemap::decode_slice(&files.blobs.read_bytes(sha256, MAX_MAP_BYTES)).ok())
}

/// The last `sourceMappingURL` comment in a script: the one that counts, as
/// a bundler appends it last.
fn source_mapping_url(source: &str) -> Option<&str> {
    let at = ["//# sourceMappingURL=", "//@ sourceMappingURL="].iter().filter_map(|marker| source.rfind(marker).map(|at| at + marker.len())).max()?;
    let url = source[at..].lines().next().unwrap_or_default().trim();
    (!url.is_empty()).then_some(url)
}

/// A map's URL, relative to the script that named it, as a path in the same
/// site — or `None` for one somewhere else.
fn resolve(script_path: &str, reference: &str) -> Option<String> {
    let base = url::Url::parse(&format!("https://site.invalid{script_path}")).ok()?;
    let resolved = base.join(reference).ok()?;
    (resolved.host_str() == Some("site.invalid")).then(|| normalize_site_path(resolved.path()))
}

/// A source map's name for a file, as a person would write it: without the
/// bundler's scheme and project prefix or the climb out of its output
/// directory.
fn source_name(source: &str) -> String {
    let mut name = match source.split_once("://") {
        // `webpack://app/./src/x.js`: the part after the project name.
        Some((_, rest)) => rest.split_once('/').map_or(rest, |(_, path)| path),
        None => source,
    };
    loop {
        let trimmed = name.trim_start_matches("./").trim_start_matches("../").trim_start_matches('/');
        if trimmed == name {
            break;
        }
        name = trimmed;
    }
    name.to_string()
}

fn context(view: &SourceView, line: u32) -> Option<Context> {
    let text = |n: u32| view.get_line(n).map(|l| l.trim_end_matches('\r'));
    let current = text(line)?;
    if current.chars().count() > MAX_CONTEXT_LINE {
        return None;
    }
    let short = |l: &str| l.chars().count() <= MAX_CONTEXT_LINE;
    let pre = (line.saturating_sub(CONTEXT_LINES)..line).filter_map(text).map(|l| if short(l) { clean(l, MAX_CONTEXT_LINE) } else { String::new() }).collect();
    let mut post: Vec<String> = (line + 1..=line + CONTEXT_LINES).map_while(text).map(|l| if short(l) { clean(l, MAX_CONTEXT_LINE) } else { String::new() }).collect();
    // A file ending in a newline has an empty line after it, which is no line.
    if line as usize + post.len() + 1 == view.line_count() && post.last().is_some_and(String::is_empty) {
        post.pop();
    }
    Some(Context { pre, line: clean(current, MAX_CONTEXT_LINE), post })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::db::testing::temp_db;
    use crate::errors::envelope::Frame;
    use crate::nsite_address::NsiteAddress;
    use crate::sites::testing::{insert_host, insert_version};

    /// `function render(props) {\n  return props.user.name;\n}\nrender({});`
    /// from `src/App.js`, minified to one line by hand, and its map.
    pub const SOURCE: &str = "function render(props) {\n  return props.user.name;\n}\nrender({});\n";
    pub const MINIFIED: &str = "function n(r){return r.user.name}n({});\n//# sourceMappingURL=app.js.map\n";

    /// A map placing `n`, `r.user.name` and the call `n({})` back in [`SOURCE`].
    pub fn map() -> String {
        let mut builder = sourcemap::SourceMapBuilder::new(Some("app.js"));
        let src = builder.add_source("webpack://site/./src/App.js");
        builder.set_source_contents(src, Some(SOURCE));
        // function n(…) -> function render
        builder.add(0, 0, 0, 0, Some("webpack://site/./src/App.js"), None, false);
        builder.add(0, 9, 0, 9, Some("webpack://site/./src/App.js"), Some("render"), false);
        builder.add(0, 11, 0, 16, Some("webpack://site/./src/App.js"), Some("props"), false);
        // return r.user.name -> line 2
        builder.add(0, 14, 1, 2, Some("webpack://site/./src/App.js"), None, false);
        builder.add(0, 21, 1, 9, Some("webpack://site/./src/App.js"), Some("props"), false);
        builder.add(0, 23, 1, 15, Some("webpack://site/./src/App.js"), Some("user"), false);
        // n({}) -> render({}) on line 4
        builder.add(0, 33, 3, 0, Some("webpack://site/./src/App.js"), Some("render"), false);
        let mut out = Vec::new();
        builder.into_sourcemap().to_writer(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn frame(path: &str, function: &str, line: u32, column: u32) -> Frame {
        Frame { abs_path: path.into(), function: Some(function.into()), lineno: Some(line), colno: Some(column), ..Frame::default() }
    }

    #[test]
    fn maps_frames_through_deployed_maps() {
        let t = temp_db();
        let blobs = BlobStore::new(&t.dir.path().join("blobs"));
        let map = map();
        let put = |bytes: &[u8]| {
            let sha = BlobHash::of(bytes);
            blobs.put(sha, bytes).unwrap();
            sha.to_string()
        };
        let (js, map_sha, plain) = (put(MINIFIED.as_bytes()), put(map.as_bytes()), put(b"let a = 1;\nthrow new Error('x');\n"));
        let address = NsiteAddress { pubkey: "a".repeat(64), kind: 15128, identifier: String::new() };
        let host_id = t.db.with(|conn| {
            let id = insert_host(conn, "docs.example.com", &address);
            insert_version(conn, &"e".repeat(64), &address, 10, &[("/assets/app.js", &js), ("/assets/app.js.map", &map_sha), ("/plain.js", &plain)]);
            Ok(id)
        }).unwrap();

        let symbolicator = Symbolicator::new(t.db.clone(), blobs.clone());
        let mut report = Report {
            release: Some("e".repeat(64)),
            exceptions: vec![Exception {
                kind: "TypeError".into(),
                frames: vec![
                    frame("https://cdn.example.com/lib.js", "x", 1, 1),
                    frame("https://docs.example.com/assets/app.js", "", 1, 34),
                    frame("https://docs.example.com/assets/app.js", "n", 1, 22),
                    frame("https://docs.example.com/plain.js", "", 2, 1),
                ],
                ..Exception::default()
            }],
            ..Report::default()
        };
        report.exceptions[0].frames[1].function = None;
        symbolicator.symbolicate(&mut report, host_id, "docs.example.com").unwrap();

        let frames = &report.exceptions[0].frames;
        assert_eq!(frames.iter().map(|f| f.in_app).collect::<Vec<_>>(), [false, true, true, true]);
        assert_eq!(frames[0].mapped, None);
        assert_eq!(frames[1].mapped, Some(Mapped { source: "src/App.js".into(), function: None, lineno: 4, colno: 1 }));
        assert_eq!(frames[2].mapped, Some(Mapped { source: "src/App.js".into(), function: Some("render".into()), lineno: 2, colno: 10 }));
        let context = frames[2].context.as_ref().unwrap();
        assert_eq!((context.pre.as_slice(), context.line.as_str()), (&["function render(props) {".to_string()][..], "  return props.user.name;"));
        assert_eq!(context.post, ["}", "render({});"]);
        // No map: the frame stays as it was, with the lines around it.
        assert_eq!(frames[3].mapped, None);
        assert_eq!(frames[3].context.as_ref().unwrap().line, "throw new Error('x');");

        // A release that isn't this site's is no way into another site's files.
        let mut other = report.clone();
        other.release = Some("f".repeat(64));
        for frame in &mut other.exceptions[0].frames {
            frame.mapped = None;
        }
        symbolicator.symbolicate(&mut other, host_id, "docs.example.com").unwrap();
        // It falls back to what the host serves, which here is the same version.
        assert!(other.exceptions[0].frames[2].mapped.is_some());
    }

    #[test]
    fn names_sources() {
        assert_eq!(source_name("webpack://site/./src/App.tsx"), "src/App.tsx");
        assert_eq!(source_name("../../src/main.ts"), "src/main.ts");
        assert_eq!(source_name("/src/x.js"), "src/x.js");
        assert_eq!(source_name("src/x.js"), "src/x.js");
        assert_eq!(resolve("/assets/app.js", "app.js.map").as_deref(), Some("/assets/app.js.map"));
        assert_eq!(resolve("/assets/app.js", "../maps/app.js.map").as_deref(), Some("/maps/app.js.map"));
        assert_eq!(resolve("/assets/app.js", "https://elsewhere.example.com/app.js.map"), None);
        assert_eq!(source_mapping_url("x\n//# sourceMappingURL=a.map\n//# sourceMappingURL=b.map  \n"), Some("b.map"));
        assert_eq!(source_mapping_url("x"), None);
        assert_eq!(site_path("https://Docs.example.com/a%20b.js", "docs.example.com").as_deref(), Some("/a b.js"));
        assert_eq!(site_path("https://docs.example.com.evil/a.js", "docs.example.com"), None);
    }
}
