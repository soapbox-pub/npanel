//! Link previews a site draws for itself.
//!
//! A site publishes `/.npanel/<kind>.jinja`, one file per kind of event it
//! wants to preview its own way, and any fonts its images need beside them,
//! as `/.npanel/*.ttf` or `*.otf`. Each template has three optional parts:
//!
//! - JSON front matter, between `---` lines: named NIP-01 filters, whose
//!   events are all the template is given. npanel fetches nothing else.
//! - Top-level `{% set %}`s of `title`, `description`, `image` and `body`,
//!   which become the page's preview.
//! - A `{% block image %}` that draws an SVG, served as a JPEG at
//!   `/_npanel/preview/<nip19>.jpg` and named as the page's `og:image`.
//!
//! The block is the template's, not the page's: every template extends an
//! empty layout, so rendering one runs its top level and skips the block,
//! and the image is the block rendered on its own afterwards, seeing the
//! same variables. A page view never pays for drawing an image.
//!
//! The files are the site's own and are never served (`sites.rs`).

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, OnceLock};

use indexmap::IndexMap;
use minijinja::value::Value;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use parking_lot::Mutex;
use regex::Regex;
use resvg::usvg::fontdb;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::helpers;
use crate::blobs::BlobStore;
use crate::db::Db;
use crate::hash::BlobHash;
use crate::nostr::pool::Filter;
use crate::nostr::{is_hex64, Event};

/// Where a site keeps its templates and fonts.
pub const DIR: &str = "/.npanel/";

/// The most events all of one template's queries are given, together.
pub const MAX_RESULTS: usize = 10;
/// The most queries one template may make.
const MAX_QUERIES: usize = 4;
/// The most values in one of a filter's lists.
const MAX_VALUES: usize = 20;
/// The longest tag value a filter may name.
const MAX_TAG_VALUE: usize = 1024;

const MAX_TEMPLATE_BYTES: u64 = 256 * 1024;
const MAX_FONT_BYTES: u64 = 32 << 20;
const MAX_FONTS: usize = 8;

/// What one render may spend: enough to read ten events' tags many times
/// over, and far short of a template that loops forever.
const FUEL: u64 = 2_000_000;
/// The largest preview body, and the largest image, a template may write.
const MAX_BODY: usize = 256 * 1024;
const MAX_SVG: usize = 4 << 20;

/// What every template extends. Empty, so its blocks only render when asked.
const LAYOUT: &str = "npanel";

/// A variable a filter can name: something about the event the page is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Var {
    Pubkey,
    Id,
    D,
    A,
}

impl Var {
    fn parse(value: &str) -> Option<Var> {
        match value {
            "$pubkey" => Some(Var::Pubkey),
            "$id" => Some(Var::Id),
            "$d" => Some(Var::D),
            "$a" => Some(Var::A),
            _ => None,
        }
    }
}

/// One value in a filter's list: as written, or a variable.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Term {
    Literal(String),
    Var(Var),
}

/// One named filter from a template's front matter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub name: String,
    ids: Vec<Term>,
    authors: Vec<Term>,
    kinds: Vec<i64>,
    tags: BTreeMap<char, Vec<Term>>,
    since: Option<i64>,
    until: Option<i64>,
    /// What the template asked for. The relays are asked for no more than
    /// what is left of [`MAX_RESULTS`], whatever it says.
    pub limit: Option<usize>,
}

impl Query {
    /// The variables it names.
    pub fn vars(&self) -> impl Iterator<Item = Var> + '_ {
        self.ids.iter().chain(&self.authors).chain(self.tags.values().flatten()).filter_map(|term| match term {
            Term::Var(var) => Some(*var),
            Term::Literal(_) => None,
        })
    }

    /// The filter to send, with its variables filled in, or `None` when one
    /// of them has no value for this event.
    pub fn resolve(&self, subject: &Subject, limit: usize) -> Option<Filter> {
        let fill = |terms: &[Term]| terms.iter().map(|term| subject.fill(term)).collect::<Option<Vec<String>>>();
        let mut d = Vec::new();
        let mut tags = BTreeMap::new();
        for (letter, terms) in &self.tags {
            let values = fill(terms)?;
            if *letter == 'd' {
                d = values;
            } else {
                tags.insert(*letter, values);
            }
        }
        Some(Filter {
            ids: fill(&self.ids)?,
            authors: fill(&self.authors)?,
            kinds: self.kinds.clone(),
            d,
            tags,
            since: self.since,
            until: self.until,
            limit: Some(limit as u64),
        })
    }
}

/// What a page is of, as far as filters and templates know it: whatever its
/// path says, and whatever the event says if it had to be fetched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub kind: i64,
    pub pubkey: Option<String>,
    pub id: Option<String>,
    pub d: Option<String>,
}

impl Subject {
    pub fn get(&self, var: Var) -> Option<String> {
        match var {
            Var::Pubkey => self.pubkey.clone(),
            Var::Id => self.id.clone(),
            Var::D => self.d.clone(),
            Var::A => {
                let pubkey = self.pubkey.as_ref()?;
                match self.kind {
                    0 | 3 | 10_000..20_000 => Some(format!("{}:{pubkey}:", self.kind)),
                    30_000..40_000 => Some(format!("{}:{pubkey}:{}", self.kind, self.d.as_ref()?)),
                    _ => None,
                }
            }
        }
    }

    fn fill(&self, term: &Term) -> Option<String> {
        match term {
            Term::Literal(value) => Some(value.clone()),
            Term::Var(var) => self.get(*var),
        }
    }

    /// Its NIP-19 name, with no relay hints, as one profile, note or address
    /// is named however its page was reached.
    pub fn nip19(&self) -> Option<String> {
        use crate::nostr::nip19;
        let kind = u32::try_from(self.kind).ok()?;
        match (self.kind, &self.pubkey, &self.id) {
            (0, Some(pubkey), _) => Some(nip19::npub_encode(pubkey)),
            (3 | 10_000..20_000, Some(pubkey), _) => Some(nip19::naddr_encode(pubkey, kind, "", &[])),
            (30_000..40_000, Some(pubkey), _) => Some(nip19::naddr_encode(pubkey, kind, self.d.as_deref()?, &[])),
            (_, _, Some(id)) => Some(nip19::note_encode(id)),
            _ => None,
        }
    }
}

/// Every site's queries for one build, by the kind of event they're for.
/// Kinds with a template and no queries are here too, with none: whether a
/// kind has a template at all decides how its pages are looked up.
#[derive(Debug, Default)]
pub struct SiteQueries {
    /// Names what is asked, so lookups for two sites asking the same thing
    /// share a cache entry, and a site that changes its queries shares
    /// nothing with what it asked before.
    pub digest: String,
    pub kinds: BTreeMap<i64, Vec<Query>>,
}

/// One kind's template, compiled.
struct KindTemplate {
    sha256: BlobHash,
    image: bool,
}

/// A build's templates, ready to render.
pub struct SiteTemplates {
    env: Environment<'static>,
    kinds: BTreeMap<i64, KindTemplate>,
    pub queries: Arc<SiteQueries>,
    pub fonts: Arc<Fonts>,
}

/// What a template's top level set, of what npanel reads. The outer
/// `Option` is whether it was set at all: `image` set to `none` turns the
/// generated image off, where leaving it out keeps it.
#[derive(Debug, Default, PartialEq)]
pub struct Fields {
    pub title: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub image: Option<Option<String>>,
    pub body: Option<Option<String>>,
}

/// What a template is given.
#[derive(Serialize)]
pub struct Context<'a> {
    pub entity: EntityContext,
    pub results: &'a IndexMap<String, Vec<Event>>,
    /// The page's URL, and its site's origin.
    pub url: &'a str,
    pub origin: &'a str,
}

#[derive(Serialize)]
pub struct EntityContext {
    pub nip19: Option<String>,
    pub kind: i64,
    pub pubkey: Option<String>,
    pub id: Option<String>,
    pub d: Option<String>,
}

impl EntityContext {
    pub fn of(subject: &Subject) -> EntityContext {
        EntityContext { nip19: subject.nip19(), kind: subject.kind, pubkey: subject.pubkey.clone(), id: subject.id.clone(), d: subject.d.clone() }
    }
}

impl SiteTemplates {
    pub fn has(&self, kind: i64) -> bool {
        self.kinds.contains_key(&kind)
    }

    pub fn has_image(&self, kind: i64) -> bool {
        self.kinds.get(&kind).is_some_and(|k| k.image)
    }

    /// The published template a kind's previews come from.
    pub fn sha256(&self, kind: i64) -> Option<BlobHash> {
        self.kinds.get(&kind).map(|k| k.sha256)
    }

    /// The preview fields a kind's template sets. Blocking.
    pub fn render_data(&self, kind: i64, context: &Context) -> Result<Fields, minijinja::Error> {
        let template = self.env.get_template(&name(kind))?;
        let captured = template.render_captured(Value::from_serialize(context))?;
        let state = captured.state();
        let read = |key: &str| -> Option<Option<String>> {
            if !state.exports().contains(&key) {
                return None;
            }
            let value = state.lookup(key)?;
            if value.is_none() || value.is_undefined() {
                return Some(None);
            }
            // A body set with `{% set body %}` is markup the template wrote,
            // with what it interpolated escaped; one set to a plain string is
            // text, and escaped here.
            let text = if key == "body" && !value.is_safe() { super::render::escape(&value.to_string()) } else { value.to_string() };
            Some(Some(text).filter(|t| !t.trim().is_empty()))
        };
        let fields = Fields { title: read("title"), description: read("description"), image: read("image"), body: read("body") };
        if fields.body.as_ref().and_then(Option::as_ref).is_some_and(|body| body.len() > MAX_BODY) {
            return Err(minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, "the body is larger than 256 KB"));
        }
        Ok(fields)
    }

    /// The SVG a kind's template draws. Blocking.
    pub fn render_image(&self, kind: i64, context: &Context) -> Result<String, minijinja::Error> {
        let template = self.env.get_template(&name(kind))?;
        let mut captured = template.render_captured(Value::from_serialize(context))?;
        let svg = captured.with_state_mut(|state| state.render_block("image"))?;
        if svg.len() > MAX_SVG {
            return Err(minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, "the image is larger than 4 MB of SVG"));
        }
        Ok(svg)
    }
}

/// A site's fonts, read only when something draws text. Mapped rather than
/// copied: an emoji font is ten megabytes.
pub struct Fonts {
    blobs: BlobStore,
    files: Vec<BlobHash>,
    db: OnceLock<Arc<fontdb::Database>>,
    /// Glyph masks drawn already, since every image of one shape draws the
    /// same one.
    pub(super) masks: Mutex<lru::LruCache<(String, u32), Option<String>>>,
}

impl Fonts {
    pub(super) fn new(blobs: BlobStore, files: Vec<BlobHash>) -> Fonts {
        Fonts { blobs, files, db: OnceLock::new(), masks: Mutex::new(lru::LruCache::new(NonZeroUsize::new(64).unwrap())) }
    }

    /// Every face the site's fonts hold. Blocking, the first time.
    pub fn db(&self) -> Arc<fontdb::Database> {
        self.db
            .get_or_init(|| {
                let mut db = fontdb::Database::new();
                for sha256 in &self.files {
                    if let Ok(bytes) = self.blobs.open(*sha256, None) {
                        db.load_font_source(fontdb::Source::Binary(Arc::new(bytes)));
                    }
                }
                Arc::new(db)
            })
            .clone()
    }

    /// The family text is set in when a template names none: the first
    /// font's. Others are fallbacks for what it lacks.
    pub fn default_family(&self) -> Option<String> {
        self.db().faces().next().and_then(|face| face.families.first()).map(|(family, _)| family.clone())
    }
}

fn name(kind: i64) -> String {
    format!("{kind}.jinja")
}

/// Something wrong with a site's templates, for its deploy log.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

/// What a build's templates come to, and what was wrong with any of them.
pub struct Compiled {
    pub templates: Option<SiteTemplates>,
    pub problems: Vec<Problem>,
}

/// Compile the files under [`DIR`] in a build. A template that doesn't
/// compile is left out, and the rest go on. Blocking.
pub fn compile(files: &[(String, BlobHash)], blobs: &BlobStore) -> Compiled {
    let mut problems = Vec::new();
    let mut problem = |path: &str, message: String| problems.push(Problem { path: path.to_string(), message });

    let mut env = environment();
    let mut kinds = BTreeMap::new();
    let mut queries = BTreeMap::new();
    let mut fonts = Vec::new();
    let mut digest = Sha256::new();

    for (path, sha256) in files {
        let Some(file) = path.strip_prefix(DIR) else { continue };
        let lower = file.to_ascii_lowercase();
        if lower.ends_with(".ttf") || lower.ends_with(".otf") {
            match blobs.size(*sha256) {
                Some(size) if size > MAX_FONT_BYTES => problem(path, "This font is larger than 32 MB, so it isn't loaded.".into()),
                Some(_) if fonts.len() >= MAX_FONTS => problem(path, "A site may have 8 fonts at most, so this one isn't loaded.".into()),
                Some(_) => fonts.push(*sha256),
                None => problem(path, "This font is missing from local storage.".into()),
            }
            continue;
        }
        let Some(kind) = template_kind(file) else {
            problem(path, "Only <kind>.jinja templates and .ttf or .otf fonts belong in /.npanel/, so this file is ignored.".into());
            continue;
        };

        if blobs.size(*sha256).is_none_or(|size| size > MAX_TEMPLATE_BYTES) {
            problem(path, "This template is larger than 256 KB, or missing, so it isn't used.".into());
            continue;
        }
        let Ok(source) = String::from_utf8(blobs.read_bytes(*sha256, MAX_TEMPLATE_BYTES)) else {
            problem(path, "This template isn't UTF-8 text, so it isn't used.".into());
            continue;
        };
        let (front, body) = match split_front_matter(&source) {
            Ok(split) => split,
            Err(error) => {
                problem(path, error);
                continue;
            }
        };
        let (front_json, kind_queries) = match front.map(parse_queries).transpose() {
            Ok(parsed) => parsed.unwrap_or_default(),
            Err(error) => {
                problem(path, error);
                continue;
            }
        };
        let declared: usize = kind_queries.iter().map(|q| q.limit.unwrap_or(MAX_RESULTS)).sum();
        if declared > MAX_RESULTS && kind_queries.iter().all(|q| q.limit.is_some()) {
            problem(path, format!("These queries ask for {declared} events, and templates get {MAX_RESULTS} at most, so the last ones get fewer than they ask for."));
        }

        let image = has_image_block(&body);
        if let Err(error) = env.add_template_owned(name(kind), format!("{{% extends \"{LAYOUT}\" %}}{body}")) {
            problem(path, format!("This template doesn't compile: {error}"));
            continue;
        }
        digest.update(format!("{kind}\0{front_json}\0").as_bytes());
        kinds.insert(kind, KindTemplate { sha256: *sha256, image });
        queries.insert(kind, kind_queries);
    }

    if kinds.is_empty() {
        return Compiled { templates: None, problems };
    }

    let fonts = Arc::new(Fonts::new(blobs.clone(), fonts));
    helpers::register(&mut env, fonts.clone());
    let queries = Arc::new(SiteQueries { digest: hex::encode(&digest.finalize()[..12]), kinds: queries });
    Compiled { templates: Some(SiteTemplates { env, kinds, queries, fonts }), problems }
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(FUEL));
    // A template copes with whatever came back: a missing event, or a
    // field an event doesn't have, is undefined however deep it's read.
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    // An SVG's attributes and text escape as HTML's do.
    env.set_auto_escape_callback(|_| AutoEscape::Html);
    env.add_template(LAYOUT, "").expect("an empty template compiles");
    env
}

/// The kind a template's file name says it is for.
fn template_kind(file: &str) -> Option<i64> {
    let digits = file.strip_suffix(".jinja")?;
    let canonical = !digits.is_empty() && digits.len() <= 5 && digits.bytes().all(|b| b.is_ascii_digit()) && (digits == "0" || !digits.starts_with('0'));
    canonical.then(|| digits.parse::<i64>().ok()).flatten().filter(|kind| *kind <= 65_535)
}

/// A template's front matter, if it has any, and the rest of it with the
/// front matter's lines left blank, so errors name the lines as written.
fn split_front_matter(source: &str) -> Result<(Option<&str>, String), String> {
    let mut lines = source.split_inclusive('\n');
    let Some(first) = lines.next().filter(|line| line.trim_end() == "---") else {
        return Ok((None, source.to_string()));
    };
    let start = first.len();
    let mut at = start;
    let mut count = 1;
    for line in lines {
        count += 1;
        if line.trim_end() == "---" {
            let front = &source[start..at];
            return Ok((Some(front), "\n".repeat(count) + &source[at + line.len()..]));
        }
        at += line.len();
    }
    Err("The front matter opened with --- never closes. End it with a line of ---.".into())
}

/// A template's queries, from its front matter, and the front matter as
/// read, which is what names them in the digest.
fn parse_queries(front: &str) -> Result<(String, Vec<Query>), String> {
    let value: serde_json::Value = serde_json::from_str(front).map_err(|e| format!("The front matter isn't valid JSON: {e}"))?;
    let serde_json::Value::Object(map) = &value else {
        return Err("The front matter must be a JSON object of named filters.".into());
    };
    if map.len() > MAX_QUERIES {
        return Err(format!("A template may make {MAX_QUERIES} queries at most, and this one makes {}.", map.len()));
    }
    let queries = map.iter().map(|(name, filter)| parse_query(name, filter)).collect::<Result<_, _>>()?;
    Ok((value.to_string(), queries))
}

fn parse_query(name: &str, value: &serde_json::Value) -> Result<Query, String> {
    static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[a-z_][a-z0-9_]*$").unwrap());
    if !NAME.is_match(name) {
        return Err(format!("{name} can't name a query: use lowercase letters, digits and underscores, starting with a letter."));
    }
    let serde_json::Value::Object(filter) = value else {
        return Err(format!("The query {name} must be a filter: a JSON object."));
    };

    let list = |key: &str, value: &serde_json::Value| -> Result<Vec<serde_json::Value>, String> {
        let serde_json::Value::Array(values) = value else {
            return Err(format!("{key} in the query {name} must be a list."));
        };
        if values.is_empty() {
            return Err(format!("{key} in the query {name} is empty, so it would match nothing."));
        }
        if values.len() > MAX_VALUES {
            return Err(format!("{key} in the query {name} lists more than {MAX_VALUES} values."));
        }
        Ok(values.clone())
    };
    let terms = |key: &str, value: &serde_json::Value, allowed: &[Var], valid: &dyn Fn(&str) -> bool| -> Result<Vec<Term>, String> {
        list(key, value)?
            .iter()
            .map(|value| {
                let text = value.as_str().ok_or_else(|| format!("{key} in the query {name} must list strings."))?;
                if let Some(var) = Var::parse(text) {
                    if allowed.contains(&var) {
                        return Ok(Term::Var(var));
                    }
                    return Err(format!("{text} can't stand in {key} in the query {name}."));
                }
                if text.starts_with('$') {
                    return Err(format!("{text} in the query {name} isn't a variable. Use $pubkey, $id, $d or $a, as the whole value."));
                }
                if !valid(text) {
                    return Err(format!("{text} isn't a valid value for {key} in the query {name}."));
                }
                Ok(Term::Literal(text.to_string()))
            })
            .collect()
    };
    let integer = |key: &str, value: &serde_json::Value| value.as_u64().ok_or_else(|| format!("{key} in the query {name} must be a whole number."));

    let mut query = Query { name: name.to_string(), ids: vec![], authors: vec![], kinds: vec![], tags: BTreeMap::new(), since: None, until: None, limit: None };
    let hex = |text: &str| is_hex64(text) && text == text.to_ascii_lowercase();
    for (key, value) in filter {
        match key.as_str() {
            "ids" => query.ids = terms(key, value, &[Var::Id], &hex)?,
            "authors" => query.authors = terms(key, value, &[Var::Pubkey], &hex)?,
            "kinds" => {
                query.kinds = list(key, value)?
                    .iter()
                    .map(|kind| kind.as_i64().filter(|k| (0..=65_535).contains(k)).ok_or_else(|| format!("kinds in the query {name} must list numbers from 0 to 65535.")))
                    .collect::<Result<_, _>>()?;
            }
            "since" => query.since = Some(integer(key, value)? as i64),
            "until" => query.until = Some(integer(key, value)? as i64),
            "limit" => query.limit = Some(usize::try_from(integer(key, value)?).unwrap_or(usize::MAX)),
            tag if tag.len() == 2 && tag.starts_with('#') && tag.as_bytes()[1].is_ascii_alphabetic() => {
                let letter = tag.as_bytes()[1] as char;
                let values = terms(key, value, &[Var::Pubkey, Var::Id, Var::D, Var::A], &|text: &str| text.len() <= MAX_TAG_VALUE)?;
                query.tags.insert(letter, values);
            }
            other => {
                return Err(format!(
                    "{other} in the query {name} isn't a filter field npanel sends. Use ids, authors, kinds, #<letter>, since, until or limit."
                ))
            }
        }
    }
    if query.ids.is_empty() && query.authors.is_empty() && query.tags.is_empty() {
        return Err(format!("The query {name} must name ids, authors or a tag, so it asks about something in particular."));
    }
    Ok(query)
}

/// Whether a template draws an image: whether it has a block named `image`
/// outside its comments.
fn has_image_block(body: &str) -> bool {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{#.*?#\}").unwrap());
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{%[-+]?\s*block\s+image\s*[-+]?%\}").unwrap());
    BLOCK.is_match(&COMMENT.replace_all(body, ""))
}

/// The files under [`DIR`] in a build.
pub fn build_files(conn: &Connection, build_id: i64) -> rusqlite::Result<Vec<(String, BlobHash)>> {
    // Every path under the directory sorts between it and the next string
    // after its `/`, so this is a range of the primary key.
    conn.prepare_cached("SELECT path, sha256 FROM host_build_files WHERE build_id = ? AND path >= ? AND path < ? ORDER BY path")?
        .query_map(rusqlite::params![build_id, DIR, "/.npanel0"], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// Builds' templates, compiled once each. A build's files never change, so
/// neither does what they compile to.
pub struct Templates {
    db: Db,
    blobs: BlobStore,
    cache: Mutex<lru::LruCache<i64, Option<Arc<SiteTemplates>>>>,
}

/// Builds whose templates are kept compiled.
const MAX_CACHED_BUILDS: usize = 64;

impl Templates {
    pub fn new(db: Db, blobs: BlobStore) -> Templates {
        Templates { db, blobs, cache: Mutex::new(lru::LruCache::new(NonZeroUsize::new(MAX_CACHED_BUILDS).unwrap())) }
    }

    /// A build's templates, or `None` for a build without any.
    pub async fn for_build(&self, build_id: i64) -> anyhow::Result<Option<Arc<SiteTemplates>>> {
        if let Some(cached) = self.cache.lock().get(&build_id) {
            return Ok(cached.clone());
        }
        let blobs = self.blobs.clone();
        let templates = self
            .db
            .call(move |conn| {
                let files = build_files(conn, build_id)?;
                Ok(if files.is_empty() { None } else { compile(&files, &blobs).templates.map(Arc::new) })
            })
            .await?;
        self.cache.lock().put(build_id, templates.clone());
        Ok(templates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBKEY: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

    fn stored(dir: &std::path::Path, files: &[(&str, &[u8])]) -> (BlobStore, Vec<(String, BlobHash)>) {
        let blobs = BlobStore::allowing_private(dir);
        let files = files
            .iter()
            .map(|(path, bytes)| {
                let sha256 = BlobHash::of(bytes);
                blobs.put(sha256, bytes).unwrap();
                (path.to_string(), sha256)
            })
            .collect();
        (blobs, files)
    }

    #[test]
    fn reads_file_names() {
        assert_eq!(template_kind("0.jinja"), Some(0));
        assert_eq!(template_kind("36767.jinja"), Some(36767));
        assert_eq!(template_kind("65536.jinja"), None);
        assert_eq!(template_kind("01.jinja"), None);
        assert_eq!(template_kind("profile.jinja"), None);
        assert_eq!(template_kind(".jinja"), None);
    }

    #[test]
    fn splits_front_matter() {
        let (front, body) = split_front_matter("---\n{\"a\": 1}\n---\nline 4\n").unwrap();
        assert_eq!(front, Some("{\"a\": 1}\n"));
        assert_eq!(body, "\n\n\nline 4\n");
        assert_eq!(split_front_matter("no front\n").unwrap(), (None, "no front\n".into()));
        assert!(split_front_matter("---\n{}\n").is_err());
    }

    #[test]
    fn parses_queries() {
        let (_, queries) = parse_queries(r##"{"theme": {"kinds": [16767], "authors": ["$pubkey"], "limit": 1}, "reactions": {"kinds": [7], "#e": ["$id"]}}"##).unwrap();
        assert_eq!(queries.iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), ["theme", "reactions"]);
        assert_eq!(queries[0].vars().collect::<Vec<_>>(), [Var::Pubkey]);

        let subject = Subject { kind: 0, pubkey: Some(PUBKEY.into()), id: None, d: None };
        let filter = queries[0].resolve(&subject, 1).unwrap();
        assert_eq!(filter.to_json(), serde_json::json!({ "kinds": [16767], "authors": [PUBKEY], "limit": 1 }));
        // The subject's id isn't known, so the query that names it can't be made.
        assert_eq!(queries[1].resolve(&subject, 9), None);

        for (front, error) in [
            (r#"{"Theme": {"authors": ["$pubkey"]}}"#, "can't name a query"),
            (r#"{"t": {"authors": ["npub of $pubkey"]}}"#, "isn't a valid value"),
            (r#"{"t": {"authors": ["$id"]}}"#, "can't stand in authors"),
            (r#"{"t": {"authors": ["$who"]}}"#, "isn't a variable"),
            (r#"{"t": {"kinds": [1], "search": "nostr"}}"#, "isn't a filter field"),
            (r#"{"t": {"kinds": [1]}}"#, "must name ids, authors or a tag"),
            (r#"{"t": {"authors": []}}"#, "is empty"),
            (r#"{"a": {"ids": ["$id"]}, "b": {"ids": ["$id"]}, "c": {"ids": ["$id"]}, "d": {"ids": ["$id"]}, "e": {"ids": ["$id"]}}"#, "4 queries at most"),
            ("[]", "JSON object"),
        ] {
            let message = parse_queries(front).unwrap_err();
            assert!(message.contains(error), "{front}: {message}");
        }
    }

    #[test]
    fn fills_variables() {
        let article = Subject { kind: 30023, pubkey: Some(PUBKEY.into()), id: None, d: Some("post".into()) };
        assert_eq!(article.get(Var::A), Some(format!("30023:{PUBKEY}:post")));
        assert!(article.nip19().unwrap().starts_with("naddr1"));
        let note = Subject { kind: 1, pubkey: None, id: Some("a".repeat(64)), d: None };
        assert_eq!(note.get(Var::A), None);
        assert!(note.nip19().unwrap().starts_with("note1"));
        assert!(Subject { kind: 0, pubkey: Some(PUBKEY.into()), ..Default::default() }.nip19().unwrap().starts_with("npub1"));
    }

    #[test]
    fn compiles_a_sites_templates() {
        let dir = tempfile::tempdir().unwrap();
        let profile = br#"---
{ "profile": { "kinds": [0], "authors": ["$pubkey"], "limit": 1 } }
---
{%- set p = (results.profile | first).content | fromjson -%}
{%- set title = (p.name or "Someone") ~ " on Test" -%}
{%- if not p.picture %}{% set image = none %}{% endif -%}
{% block image %}<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><title>{{ p.name }}</title></svg>{% endblock %}"#;
        let (blobs, files) = stored(
            dir.path(),
            &[
                ("/.npanel/0.jinja", profile),
                ("/.npanel/1.jinja", b"{% set description = 'A note.' %}{% set body = '<b>bold</b>' %}"),
                ("/.npanel/2.jinja", b"{% if %}"),
                ("/.npanel/notes.txt", b"hi"),
            ],
        );
        let compiled = compile(&files, &blobs);
        let paths: Vec<&str> = compiled.problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["/.npanel/2.jinja", "/.npanel/notes.txt"], "{:?}", compiled.problems);
        let templates = compiled.templates.unwrap();
        assert!(templates.has(0) && templates.has(1) && !templates.has(2));
        assert!(templates.has_image(0) && !templates.has_image(1));
        assert_eq!(templates.queries.kinds[&0].len(), 1);
        assert!(templates.queries.kinds[&1].is_empty());

        let profile = Event::sign("0000000000000000000000000000000000000000000000000000000000000003", 1, 0, vec![], r#"{"name":"<Alex>"}"#);
        let results = IndexMap::from([("profile".to_string(), vec![profile])]);
        let subject = Subject { kind: 0, pubkey: Some(PUBKEY.into()), ..Default::default() };
        let context = Context { entity: EntityContext::of(&subject), results: &results, url: "https://x.example/npub1", origin: "https://x.example" };
        let fields = templates.render_data(0, &context).unwrap();
        assert_eq!(fields, Fields { title: Some(Some("<Alex> on Test".into())), description: None, image: Some(None), body: None });
        let svg = templates.render_image(0, &context).unwrap();
        assert!(svg.contains("<title>&lt;Alex&gt;</title>"), "{svg}");

        // A body set to a string is text.
        let fields = templates.render_data(1, &context).unwrap();
        assert_eq!(fields.body, Some(Some("&lt;b&gt;bold&lt;/b&gt;".into())));
        assert!(templates.render_image(1, &context).is_err());

        // Nothing comes back for a profile nobody found, and the template
        // copes.
        let empty = IndexMap::from([("profile".to_string(), vec![])]);
        let context = Context { results: &empty, ..context };
        assert_eq!(templates.render_data(0, &context).unwrap().title, Some(Some("Someone on Test".into())));
    }

    #[test]
    fn runs_out_of_fuel() {
        let dir = tempfile::tempdir().unwrap();
        let (blobs, files) = stored(dir.path(), &[("/.npanel/1.jinja", b"{% for a in range(100000) %}{% for b in range(100000) %}{% endfor %}{% endfor %}")]);
        let templates = compile(&files, &blobs).templates.unwrap();
        let empty = IndexMap::new();
        let context = Context { entity: EntityContext::of(&Subject::default()), results: &empty, url: "", origin: "" };
        let error = templates.render_data(1, &context).unwrap_err();
        assert_eq!(error.kind(), minijinja::ErrorKind::OutOfFuel);
    }

    #[test]
    fn finds_image_blocks() {
        assert!(has_image_block("{% block image %}<svg/>{% endblock %}"));
        assert!(has_image_block("{%- block image -%}<svg/>{% endblock %}"));
        assert!(!has_image_block("{# {% block image %} #}"));
        assert!(!has_image_block("{% block images %}{% endblock %}"));
    }
}
