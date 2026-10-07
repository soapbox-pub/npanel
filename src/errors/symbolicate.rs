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
//!
//! Anyone can send a report, so what one costs is bounded: a few files read
//! per report, scripts and pages only, one file parsed at a time across all
//! of them, and npanel's database consulted only to say which files those
//! are — never held while they are read.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, OnceLock};

use lru::LruCache;
use parking_lot::Mutex;
use rusqlite::Connection;
use sourcemap::{DecodedMap, SourceView};

use super::envelope::{clean, Context, Exception, Mapped, Report};
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::hash::BlobHash;
use crate::nostr::is_hex64;
use crate::sites::{active_event, host_by_id, newest_by_path, normalize_site_path, site_event, version_chain};

/// Largest script read to map a frame in it.
const MAX_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;
/// Largest source map read.
const MAX_MAP_BYTES: u64 = 64 * 1024 * 1024;
/// Scripts kept parsed, and maps. A decoded map of a large bundle is tens of
/// megabytes, so only a few.
const MAX_CACHED: usize = 8;
/// Frames mapped per report: the site's own, innermost first.
const MAX_MAPPED_FRAMES: usize = 100;
/// Files read per report. A real stack trace names a handful; one naming
/// more is asking npanel to read the whole site.
const MAX_SCRIPTS: usize = 8;
/// Lines of context either side of a frame.
const CONTEXT_LINES: u32 = 5;
/// Longest line shown as context. Past it, a line is a minified bundle's.
const MAX_CONTEXT_LINE: usize = 300;

/// A script, and where its source map is.
struct Script {
    view: SourceView,
    map: MapSource,
}

enum MapSource {
    /// Inline, as a `data:` URL: part of the script, so kept with it.
    Inline(Option<Arc<DecodedMap>>),
    /// The `sourceMappingURL` it names, relative to wherever it is found.
    Named(String),
    /// None named: the file beside it with `.map` appended, if there is one.
    Beside,
}

/// A cache entry, filled once by whichever report needs it first. The
/// others wanting it wait for that rather than parse it again.
type Slot<T> = Arc<OnceLock<Option<Arc<T>>>>;

pub struct Symbolicator {
    db: Db,
    blobs: BlobStore,
    /// By the script's own hash: everything kept is the script's.
    scripts: Mutex<LruCache<BlobHash, Slot<Script>>>,
    /// By the map's own hash. Which map a script has depends on the version
    /// and path it is found at, not on its bytes.
    maps: Mutex<LruCache<BlobHash, Slot<DecodedMap>>>,
    /// Held while a file is read and parsed, so a burst of reports — every
    /// visitor's, after a broken deploy — parses each file once and one at a
    /// time, rather than once per report all at once.
    parsing: Mutex<()>,
}

/// A file a report's frames are in, ready to map them.
struct File {
    script: Arc<Script>,
    map: Option<Arc<DecodedMap>>,
}

impl Symbolicator {
    pub fn new(db: Db, blobs: BlobStore) -> Symbolicator {
        let size = NonZeroUsize::new(MAX_CACHED).unwrap();
        Symbolicator { db, blobs, scripts: Mutex::new(LruCache::new(size)), maps: Mutex::new(LruCache::new(size)), parsing: Mutex::new(()) }
    }

    /// Mark which of the report's frames are the site's own — served from
    /// `hostname`, the name the report arrived on — and map them. Blocking.
    pub fn symbolicate(&self, report: &mut Report, host_id: i64, hostname: &str) -> anyhow::Result<()> {
        for exception in &mut report.exceptions {
            for frame in &mut exception.frames {
                frame.in_app = site_path(&frame.abs_path, hostname).is_some();
            }
        }
        let paths = scripts_named(report, hostname);
        if paths.is_empty() {
            return Ok(());
        }

        // Which files those are, and the connection given back before any
        // of them is read.
        let release = report.release.clone();
        let found = self.db.with(|conn| {
            let Some(chain) = chain(conn, host_id, release.as_deref())? else { return Ok(None) };
            let mut scripts = Vec::new();
            for path in paths {
                if let Some(sha256) = find(conn, &chain, &path)? {
                    scripts.push((path, sha256));
                }
            }
            Ok(Some((chain, scripts)))
        })?;
        let Some((chain, scripts)) = found else { return Ok(()) };

        let scripts: Vec<(String, Arc<Script>)> = scripts.into_iter().filter_map(|(path, sha256)| Some((path, self.script(sha256)?))).collect();
        // Where their maps are, the same way.
        let maps: Vec<Option<BlobHash>> = self.db.with(|conn| {
            Ok(scripts
                .iter()
                .map(|(path, script)| match &script.map {
                    MapSource::Inline(_) => Ok(None),
                    MapSource::Named(url) => resolve(path, url).map_or(Ok(None), |map_path| find(conn, &chain, &map_path)),
                    MapSource::Beside => find(conn, &chain, &format!("{path}.map")),
                })
                .collect::<rusqlite::Result<_>>()?)
        })?;
        let files: HashMap<String, File> = scripts
            .into_iter()
            .zip(maps)
            .map(|((path, script), map_sha256)| {
                let map = match &script.map {
                    MapSource::Inline(map) => map.clone(),
                    MapSource::Named(_) | MapSource::Beside => map_sha256.and_then(|sha256| self.map(sha256)),
                };
                (path, File { script, map })
            })
            .collect();

        let mut budget = MAX_MAPPED_FRAMES;
        for exception in report.exceptions.iter_mut().rev() {
            map_exception(exception, hostname, &files, &mut budget);
        }
        Ok(())
    }

    fn script(&self, sha256: BlobHash) -> Option<Arc<Script>> {
        self.cached(&self.scripts, sha256, || load_script(&self.blobs, sha256))
    }

    fn map(&self, sha256: BlobHash) -> Option<Arc<DecodedMap>> {
        self.cached(&self.maps, sha256, || load_map(&self.blobs, sha256))
    }

    /// What `cache` has for `sha256`, loaded if nothing has asked for it yet.
    /// The cache is only locked to find the entry: a large map takes a while
    /// to parse, and a report whose files are already parsed shouldn't wait.
    fn cached<T>(&self, cache: &Mutex<LruCache<BlobHash, Slot<T>>>, sha256: BlobHash, load: impl FnOnce() -> Option<T>) -> Option<Arc<T>> {
        let slot = cache.lock().get_or_insert(sha256, Default::default).clone();
        slot.get_or_init(|| {
            let _parsing = self.parsing.lock();
            load().map(Arc::new)
        })
        .clone()
    }
}

/// The versions a report's frames are looked up in: the chain of the
/// version it names, if that is one of this host's site's ready versions.
/// A Sentry SDK names a release of its own, or none, and gets the version
/// the host serves now. A version npanel no longer has, or never had for
/// this site, maps nothing: the frames' lines and columns are that
/// version's, and another version's map would put them somewhere they
/// aren't.
fn chain(conn: &Connection, host_id: i64, release: Option<&str>) -> rusqlite::Result<Option<Vec<String>>> {
    let Some(host) = host_by_id(conn, host_id)? else { return Ok(None) };
    let event = match release {
        Some(release) if is_hex64(release) => site_event(conn, release)?.filter(|e| e.address() == host.address() && e.status == crate::db::DeployStatus::Ready),
        _ => active_event(conn, &host)?,
    };
    let Some(event) = event else { return Ok(None) };
    Ok(Some(version_chain(conn, &host, &event)?))
}

/// The blob a site path is in the chain, if any version has it.
fn find(conn: &Connection, chain: &[String], path: &str) -> rusqlite::Result<Option<BlobHash>> {
    let placeholders = vec!["?"; chain.len()].join(", ");
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&path];
    values.extend(chain.iter().map(|id| id as &dyn rusqlite::ToSql));
    let rows: Vec<(String, BlobHash, String)> = conn
        .prepare_cached(&format!("SELECT path, sha256, event_id FROM site_files WHERE path = ? AND event_id IN ({placeholders})"))?
        .query_map(values.as_slice(), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(newest_by_path(rows, chain, |row| (row.0.as_str(), row.2.as_str())).into_values().next().map(|row| row.1))
}

/// A frame URL's path within the site, if it is on the site's own origin.
fn site_path(abs_path: &str, hostname: &str) -> Option<String> {
    let url = url::Url::parse(abs_path).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.host_str().is_some_and(|h| h.eq_ignore_ascii_case(hostname)) {
        return None;
    }
    Some(normalize_site_path(url.path()))
}

/// The files of the site a report's frames are in, innermost first, as
/// many as are read for one report. Only scripts and pages — a page for its
/// inline scripts — since nothing else throws.
fn scripts_named(report: &Report, hostname: &str) -> Vec<String> {
    let frames = report.exceptions.iter().rev().flat_map(|e| e.frames.iter().rev()).filter(|f| f.in_app && f.lineno.is_some()).take(MAX_MAPPED_FRAMES);
    let mut paths = Vec::new();
    for path in frames.filter_map(|frame| site_path(&frame.abs_path, hostname)) {
        let script = crate::mime::content_type(&path).starts_with("text/javascript") || crate::mime::is_html(&path);
        if !script || paths.contains(&path) {
            continue;
        }
        if paths.len() == MAX_SCRIPTS {
            break;
        }
        paths.push(path);
    }
    paths
}

fn map_exception(exception: &mut Exception, hostname: &str, files: &HashMap<String, File>, budget: &mut usize) {
    // Innermost first, so a long stack spends the budget where it matters.
    for index in (0..exception.frames.len()).rev() {
        if *budget == 0 {
            break;
        }
        let frame = &exception.frames[index];
        let (Some(path), Some(lineno)) = (frame.in_app.then(|| site_path(&frame.abs_path, hostname)).flatten(), frame.lineno) else { continue };
        *budget -= 1;
        let Some(file) = files.get(&path) else { continue };

        let (line, column) = (lineno - 1, frame.colno.unwrap_or(1) - 1);
        // What the frame's caller was calling at the moment: the original
        // name of this frame's function, when nothing better is known.
        let caller_name = index
            .checked_sub(1)
            .map(|caller| &exception.frames[caller])
            .filter(|caller| caller.abs_path == exception.frames[index].abs_path)
            .and_then(|caller| Some((caller.lineno? - 1, caller.colno.unwrap_or(1) - 1)))
            .and_then(|(l, c)| file.map.as_ref()?.lookup_token(l, c)?.get_name().map(str::to_string));

        let frame = &mut exception.frames[index];
        let token = file.map.as_ref().and_then(|map| map.lookup_token(line, column)).filter(|t| t.has_source());
        match token {
            Some(token) => {
                let minified = frame.function.as_deref().and_then(|f| f.rsplit('.').next());
                let original = file.map.as_ref().and_then(|map| map.get_original_function_name(line, column, minified, Some(&file.script.view)));
                frame.mapped = Some(Mapped {
                    source: clean(&source_name(token.get_source().unwrap_or_default()), 512),
                    function: original.map(str::to_string).or(caller_name).map(|f| clean(&f, 256)),
                    lineno: token.get_src_line() + 1,
                    colno: token.get_src_col() + 1,
                });
                frame.context = token.get_source_view().and_then(|view| context(view, token.get_src_line()));
            }
            None => frame.context = context(&file.script.view, line),
        }
    }
}

/// A script, and where its source map is: inline, or named by its
/// `sourceMappingURL`, or else beside it. Blocking.
fn load_script(blobs: &BlobStore, sha256: BlobHash) -> Option<Script> {
    if blobs.size(sha256).is_none_or(|size| size > MAX_SCRIPT_BYTES) {
        return None;
    }
    let source = blobs.read_text(sha256, MAX_SCRIPT_BYTES);
    let map = match source_mapping_url(&source) {
        Some(url) if url.starts_with("data:") => {
            MapSource::Inline(sourcemap::SourceMapRef::Ref(url.to_string()).get_embedded_sourcemap().ok().flatten().map(Arc::new))
        }
        Some(url) => MapSource::Named(url.to_string()),
        None => MapSource::Beside,
    };
    Some(Script { view: SourceView::from_string(source), map })
}

fn load_map(blobs: &BlobStore, sha256: BlobHash) -> Option<DecodedMap> {
    if blobs.size(sha256).is_none_or(|size| size > MAX_MAP_BYTES) {
        return None;
    }
    sourcemap::decode_slice(&blobs.read_bytes(sha256, MAX_MAP_BYTES)).ok()
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
        map_from("webpack://site/./src/App.js")
    }

    fn map_from(source: &str) -> String {
        let mut builder = sourcemap::SourceMapBuilder::new(Some("app.js"));
        let src = builder.add_source(source);
        builder.set_source_contents(src, Some(SOURCE));
        // function n(…) -> function render
        builder.add(0, 0, 0, 0, Some(source), None, false);
        builder.add(0, 9, 0, 9, Some(source), Some("render"), false);
        builder.add(0, 11, 0, 16, Some(source), Some("props"), false);
        // return r.user.name -> line 2
        builder.add(0, 14, 1, 2, Some(source), None, false);
        builder.add(0, 21, 1, 9, Some(source), Some("props"), false);
        builder.add(0, 23, 1, 15, Some(source), Some("user"), false);
        // n({}) -> render({}) on line 4
        builder.add(0, 33, 3, 0, Some(source), Some("render"), false);
        let mut out = Vec::new();
        builder.into_sourcemap().to_writer(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn frame(path: &str, function: &str, line: u32, column: u32) -> Frame {
        Frame { abs_path: path.into(), function: Some(function.into()), lineno: Some(line), colno: Some(column), ..Frame::default() }
    }

    fn unmapped(report: &Report) -> Report {
        let mut report = report.clone();
        for frame in &mut report.exceptions[0].frames {
            frame.mapped = None;
            frame.context = None;
        }
        report
    }

    #[test]
    fn maps_frames_through_deployed_maps() {
        let t = temp_db();
        let blobs = BlobStore::new(&t.dir.path().join("blobs"));
        let put = |bytes: &[u8]| {
            let sha = BlobHash::of(bytes);
            blobs.put(sha, bytes).unwrap();
            sha.to_string()
        };
        let (js, map_sha, plain) = (put(MINIFIED.as_bytes()), put(map().as_bytes()), put(b"let a = 1;\nthrow new Error('x');\n"));
        let other_map = put(map_from("webpack://other/./src/Other.js").as_bytes());
        let address = NsiteAddress { pubkey: "a".repeat(64), kind: 15128, identifier: String::new() };
        let other_address = NsiteAddress { pubkey: "b".repeat(64), ..address.clone() };
        let (host_id, other_id) = t
            .db
            .with(|conn| {
                let id = insert_host(conn, "docs.example.com", &address);
                insert_version(conn, &"e".repeat(64), &address, 10, &[("/assets/app.js", &js), ("/assets/app.js.map", &map_sha), ("/plain.js", &plain)]);
                // Another site with the very same script, beside a map of its own.
                let other = insert_host(conn, "other.example.com", &other_address);
                insert_version(conn, &"d".repeat(64), &other_address, 10, &[("/assets/app.js", &js), ("/assets/app.js.map", &other_map)]);
                Ok((id, other))
            })
            .unwrap();

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
        let fresh = report.clone();
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

        // The same script on another site goes through that site's map, not
        // the one parsed for the first.
        let mut other = Report { release: Some("d".repeat(64)), ..unmapped(&fresh) };
        for frame in &mut other.exceptions[0].frames {
            frame.abs_path = frame.abs_path.replace("docs.example.com", "other.example.com");
        }
        symbolicator.symbolicate(&mut other, other_id, "other.example.com").unwrap();
        assert_eq!(other.exceptions[0].frames[2].mapped.as_ref().unwrap().source, "src/Other.js");

        // A version this site doesn't have maps nothing, rather than placing
        // its lines in another version's files.
        let mut gone = Report { release: Some("f".repeat(64)), ..unmapped(&fresh) };
        symbolicator.symbolicate(&mut gone, host_id, "docs.example.com").unwrap();
        assert!(gone.exceptions[0].frames.iter().all(|f| f.mapped.is_none() && f.context.is_none()));
        assert!(gone.exceptions[0].frames[2].in_app);
        // Nor does another site's version.
        let mut theirs = Report { release: Some("d".repeat(64)), ..unmapped(&fresh) };
        symbolicator.symbolicate(&mut theirs, host_id, "docs.example.com").unwrap();
        assert!(theirs.exceptions[0].frames.iter().all(|f| f.mapped.is_none()));

        // A Sentry SDK's own release, or none, goes by what the host serves.
        for release in [Some("1.2.3".to_string()), None] {
            let mut sdk = Report { release, ..unmapped(&fresh) };
            symbolicator.symbolicate(&mut sdk, host_id, "docs.example.com").unwrap();
            assert!(sdk.exceptions[0].frames[2].mapped.is_some());
        }
    }

    #[test]
    fn reads_few_files_per_report() {
        let frames = (0..20).map(|i| frame(&format!("https://docs.example.com/{i}.js"), "f", 1, 1));
        let mut frames: Vec<Frame> = frames.chain([frame("https://docs.example.com/photo.jpg", "f", 1, 1), frame("https://docs.example.com/", "f", 1, 1)]).collect();
        frames.push(frame("https://docs.example.com/about.html", "f", 1, 1));
        frames.push(frame("https://docs.example.com/about.html", "g", 2, 1));
        for frame in &mut frames {
            frame.in_app = true;
        }
        let report = Report { exceptions: vec![Exception { frames, ..Exception::default() }], ..Report::default() };
        let paths = scripts_named(&report, "docs.example.com");
        // Innermost first; pages for their inline scripts, but not images.
        assert_eq!(paths.len(), MAX_SCRIPTS);
        assert_eq!(&paths[..3], ["/about.html", "/19.js", "/18.js"]);
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
