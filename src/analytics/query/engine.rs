//! Plausible's stats queries, answered from SQLite.
//!
//! A port of the semantics of Plausible's query engine rather than of its SQL,
//! which is ClickHouse's. What it keeps is what the numbers mean:
//!
//! - A visit belongs to a range if it was active in it, and is bucketed by
//!   when it was last active — or, by the hour and minute, counted in every
//!   bucket it was active in.
//! - Visitors and visits are counted from pageviews when the question is about
//!   pages, and from visits otherwise.
//! - A visit-level metric broken down or filtered by page is about the visits
//!   that *entered* on that page.
//! - Time on page and scroll depth come from engagement events, averaged over
//!   the visits that sent them.
//!
//! Each question becomes at most a few grouped queries whose rows are merged,
//! ordered and paginated here, and answered in Plausible's response shape.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::NaiveDateTime;
use chrono_tz::Tz;
use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::parse::{Dimension, Filter, Metric, Order, QueryError, Sort, StatsQuery, Subject, VisitDimension};
use super::period::*;
use super::zone::{iso_in_zone, offset_segments, parse_zone, zoned_date};
use crate::analytics::store::EventKind;

/// A site the analytics thread knows, as the answer names it.
#[derive(Clone, Debug)]
pub struct QueryHost {
    pub id: i64,
    pub hostname: String,
}

/// What went wrong answering a question: the question, or npanel.
#[derive(Debug)]
pub enum StatsError {
    Query(QueryError),
    Internal(anyhow::Error),
}

impl From<QueryError> for StatsError {
    fn from(error: QueryError) -> Self {
        StatsError::Query(error)
    }
}

impl From<rusqlite::Error> for StatsError {
    fn from(error: rusqlite::Error) -> Self {
        StatsError::Internal(error.into())
    }
}

type Result<T> = std::result::Result<T, StatsError>;

/// A number as Plausible's responses write it: whole numbers without a
/// fraction, since the dashboard shows `50` rather than `50.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number(pub f64);

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let Number(value) = *self;
        if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
            serializer.serialize_i64(value as i64)
        } else {
            serializer.serialize_f64(value)
        }
    }
}

fn numbers(values: &[Option<f64>]) -> Vec<Option<Number>> {
    values.iter().map(|value| value.map(Number)).collect()
}

/// A group's dimension values, which are text — or, where SQL found none,
/// null.
type Key = Vec<Option<String>>;

#[derive(Clone, Debug)]
struct Row {
    dimensions: Key,
    metrics: Vec<Option<f64>>,
}

/// What a grouped query counts per group, before metrics are finished. Each
/// query fills in the columns it selects.
#[derive(Clone, Debug, Default)]
struct Aggregates {
    visitors: Option<f64>,
    visits: Option<f64>,
    pageviews: Option<f64>,
    bounces: Option<f64>,
    bounce_visits: Option<f64>,
    duration: Option<f64>,
    engaged_time: Option<f64>,
    engaged_visits: Option<f64>,
    /// The sum of each visit's deepest scroll.
    total: Option<f64>,
}

impl Aggregates {
    fn column(&mut self, name: &str) -> Option<&mut Option<f64>> {
        Some(match name {
            "visitors" => &mut self.visitors,
            "visits" => &mut self.visits,
            "pageviews" => &mut self.pageviews,
            "bounces" => &mut self.bounces,
            "bounce_visits" => &mut self.bounce_visits,
            "duration" => &mut self.duration,
            "engaged_time" => &mut self.engaged_time,
            "engaged_visits" => &mut self.engaged_visits,
            "total" => &mut self.total,
            _ => return None,
        })
    }
}

type Groups = IndexMap<Key, Aggregates>;

/// One row of an answer.
#[derive(Serialize)]
pub struct ResultRow {
    pub dimensions: Key,
    pub metrics: Vec<Option<Number>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison: Option<ComparedRow>,
}

/// A row of the range compared against, and how each metric changed.
#[derive(Serialize)]
pub struct ComparedRow {
    pub dimensions: Key,
    pub metrics: Vec<Option<Number>>,
    pub change: Option<Vec<Option<Number>>>,
}

/// Everything about an answer that isn't its rows. Fields are in the order
/// Plausible's keys sort in, which is the order it sends them.
#[derive(Serialize, Default)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_partial_time_labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_time_label_result_indices: Option<Vec<Option<usize>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_time_labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_metrics: Option<Vec<Option<Number>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports_included: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_time_labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present_index: Option<Option<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_label_result_indices: Option<Vec<Option<usize>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_labels: Option<Vec<String>>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// The query as it was run, in the shape Plausible echoes it back.
#[derive(Serialize)]
pub struct Echo {
    site_id: String,
    metrics: Vec<&'static str>,
    date_range: [String; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison_date_range: Option<[String; 2]>,
    filters: Vec<Value>,
    dimensions: Vec<String>,
    order_by: Vec<[String; 2]>,
    include: EchoInclude,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagination: Option<Pagination>,
}

#[derive(Serialize)]
struct EchoInclude {
    #[serde(skip_serializing_if = "is_false")]
    imports: bool,
    #[serde(skip_serializing_if = "is_false")]
    imports_meta: bool,
    #[serde(skip_serializing_if = "is_false")]
    time_labels: bool,
    #[serde(skip_serializing_if = "is_false")]
    time_label_result_indices: bool,
    #[serde(skip_serializing_if = "is_false")]
    partial_time_labels: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    compare: Option<Value>,
    #[serde(skip_serializing_if = "is_false")]
    compare_match_day_of_week: bool,
    #[serde(skip_serializing_if = "is_false")]
    empty_metrics: bool,
    #[serde(skip_serializing_if = "is_false")]
    present_index: bool,
    trim_relative_date_range: bool,
}

#[derive(Serialize)]
struct Pagination {
    limit: i64,
    offset: i64,
}

/// An answer, in Plausible's response shape.
#[derive(Serialize)]
pub struct QueryResult {
    pub results: Vec<ResultRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_results: Option<Vec<ComparedRow>>,
    pub meta: Meta,
    pub query: Echo,
}

/// A filter suggestion: what to filter by, and what to show for it.
#[derive(Serialize)]
pub struct Suggestion {
    value: String,
    label: String,
}

/// How events reach the visit they belong to, spelled out rather than left to
/// the planner, which gets it catastrophically wrong: events are scanned by
/// site and time, and each looks up its one visit by id.
const VISITS_JOIN: &str = "CROSS JOIN sessions s INDEXED BY sessions_session_idx";
const VISITS_ON: [&str; 2] = ["s.host_id = e.host_id", "s.session_id = e.session_id"];

/// A visit longer than this is assumed not to exist, which bounds how far back
/// a range has to look for visits.
const LONGEST_VISIT: i64 = 86_400;

/// Most time buckets one query may ask for: a year by the hour, with room over.
const MAX_TIME_BUCKETS: i64 = 10_000;

fn not_set(column: &str, blank: &str) -> String {
    format!("CASE WHEN {column} = '' THEN '{blank}' ELSE {column} END")
}

/// Each visit dimension as an expression over `sessions s`, with the blank
/// values spelled the way Plausible spells them.
fn visit_sql(dimension: VisitDimension) -> String {
    use VisitDimension::*;
    match dimension {
        Source => not_set("s.source", "Direct / None"),
        Referrer => not_set("s.referrer", "Direct / None"),
        Channel => not_set("s.channel", "Direct"),
        UtmMedium => not_set("s.utm_medium", "(not set)"),
        UtmSource => not_set("s.utm_source", "(not set)"),
        UtmCampaign => not_set("s.utm_campaign", "(not set)"),
        UtmContent => not_set("s.utm_content", "(not set)"),
        UtmTerm => not_set("s.utm_term", "(not set)"),
        Country | CountryName => "s.country".into(),
        // Regions and cities are known by name here rather than by ISO code
        // or GeoNames id, which DB-IP's free database doesn't carry.
        Region => "CASE WHEN s.region = '' THEN '' ELSE s.country || '-' || s.region END".into(),
        RegionName => "s.region".into(),
        City => "CASE WHEN s.city = '' THEN '' ELSE s.country || '-' || s.region || '-' || s.city END".into(),
        CityName => "s.city".into(),
        Device => not_set("s.device", "(not set)"),
        Browser => not_set("s.browser", "(not set)"),
        BrowserVersion => not_set("s.browser_version", "(not set)"),
        Os => not_set("s.os", "(not set)"),
        OsVersion => not_set("s.os_version", "(not set)"),
        EntryPage => "s.entry_page".into(),
        ExitPage => "s.exit_page".into(),
    }
}

/// What a filter tests, as an expression over `events e` joined to
/// `sessions s`.
fn subject_sql(subject: Subject) -> String {
    match subject {
        Subject::Page => "e.pathname".into(),
        Subject::Visit(visit) => visit_sql(visit),
    }
}

/// Whether a metric is counted from visits rather than from events. Which
/// side visitors and visits come from follows the question: pageviews when it
/// is about pages, visits otherwise.
fn from_sessions(metric: Metric, pages: bool) -> bool {
    use Metric::*;
    match metric {
        BounceRate | VisitDuration | ViewsPerVisit => true,
        Visitors | Visits | Percentage | ExitRate => !pages,
        Pageviews | TimeOnPage | ScrollDepth => false,
    }
}

fn from_events(metric: Metric, pages: bool) -> bool {
    use Metric::*;
    match metric {
        Pageviews | TimeOnPage => true,
        Visitors | Visits | Percentage => pages,
        BounceRate | VisitDuration | ViewsPerVisit | ExitRate | ScrollDepth => false,
    }
}

/// What a metric reads as where there is nothing to measure.
fn empty_value(metric: Metric) -> Option<f64> {
    match metric {
        Metric::VisitDuration | Metric::ExitRate | Metric::ScrollDepth | Metric::TimeOnPage => None,
        _ => Some(0.0),
    }
}

/// Collects the parameters of a statement as it is written.
#[derive(Default)]
struct Params(Vec<SqlValue>);

impl Params {
    fn add(&mut self, value: impl Into<SqlValue>) -> &'static str {
        self.0.push(value.into());
        "?"
    }
}

/// Everything one grouped query needs to know.
struct Scope<'a> {
    host: &'a QueryHost,
    range: TimeRange,
    tz: Tz,
    interval: Option<Interval>,
    buckets: &'a [NaiveDateTime],
    dimensions: Vec<Dimension>,
    filters: &'a [Filter],
}

impl Scope<'_> {
    /// Whether any dimension or filter is about the visit, so events have to
    /// be joined to their visits.
    fn joins_visits(&self) -> bool {
        self.dimensions.iter().any(|d| matches!(d, Dimension::Visit(_))) || self.filters.iter().any(|f| matches!(f.subject, Subject::Visit(_)))
    }
}

fn country_names() -> &'static HashMap<String, String> {
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| serde_json::from_str(include_str!("../../../assets/analytics/country-names.json")).unwrap_or_default())
}

pub struct QueryEngine<'c> {
    conn: &'c Connection,
    now: i64,
}

impl<'c> QueryEngine<'c> {
    pub fn new(conn: &'c Connection, now: i64) -> Self {
        Self { conn, now }
    }

    /// Answer one dashboard query for one site, in Plausible's response shape.
    pub fn query(&self, host: &QueryHost, query: &StatsQuery, timezone: &str) -> Result<QueryResult> {
        let tz = parse_zone(timezone);
        let now = self.now;
        let ctx = self.context(host, query, tz)?;
        let interval = query.interval();
        let compare = query.include.compare.as_ref();

        let range = main_range(&query.date_range, &ctx, compare);
        let buckets = match interval {
            Some(interval) => bounded_buckets(interval, range, tz, now)?,
            None => Vec::new(),
        };
        let labels = bucket_labels(interval, &buckets);
        let order_by = query.order_by.clone().unwrap_or_else(|| default_order(query));

        let all = sort_rows(self.rows(query, &self.scope(host, query, range, tz, &buckets))?, query, &order_by);
        let (limit, offset) = query.pagination.unwrap_or((10_000, 0));
        let results: Vec<Row> = all.into_iter().skip(offset as usize).take(limit as usize).collect();
        let mut result_rows: Vec<ResultRow> =
            results.iter().map(|row| ResultRow { dimensions: row.dimensions.clone(), metrics: numbers(&row.metrics), comparison: None }).collect();

        let mut meta = Meta::default();
        if query.include.imports || query.include.imports_meta {
            meta.imports_included = Some(false);
        }
        if query.include.empty_metrics {
            meta.empty_metrics = Some(query.metrics.iter().map(|m| empty_value(*m).map(Number)).collect());
        }

        let mut comparison = None;
        let mut comparison_results = None;

        if let Some(compare) = compare {
            let comparison_range = comparison_range(&query.date_range, range, compare, query.include.compare_match_day_of_week, &ctx);
            comparison = Some(comparison_range);
            let comparison_buckets = match interval {
                Some(interval) => bounded_buckets(interval, comparison_range, tz, now)?,
                None => Vec::new(),
            };
            let comparison_labels = bucket_labels(interval, &comparison_buckets);
            let compared = self.rows(query, &self.scope(host, query, comparison_range, tz, &comparison_buckets))?;

            if let (Some(interval), 1) = (interval, query.dimensions.len()) {
                // A time series: two lists, each only its non-empty buckets,
                // with each comparison bucket's change measured against the
                // bucket it lines up with.
                let by_label: HashMap<&str, &Vec<Option<f64>>> =
                    results.iter().filter_map(|row| Some((row.dimensions[0].as_deref()?, &row.metrics))).collect();
                let line_up: HashMap<&str, &str> = comparison_labels.iter().map(String::as_str).zip(labels.iter().map(String::as_str)).collect();

                let sorted = sort_rows(compared, query, &order_by);
                let list = sorted
                    .iter()
                    .map(|row| {
                        let main = row.dimensions[0].as_deref().and_then(|label| line_up.get(label)).and_then(|label| by_label.get(label));
                        ComparedRow {
                            dimensions: row.dimensions.clone(),
                            metrics: numbers(&row.metrics),
                            change: main.map(|main| changes(&query.metrics, &row.metrics, main)),
                        }
                    })
                    .collect();

                if query.include.time_labels {
                    meta.comparison_time_label_result_indices = Some(result_indices(&comparison_labels, &sorted));
                    if query.include.partial_time_labels {
                        let partial = partial_buckets(&comparison_buckets, interval, comparison_range, tz, now);
                        meta.comparison_partial_time_labels = Some(partial.into_iter().map(|b| interval.label(b)).collect());
                    }
                    meta.comparison_time_labels = Some(comparison_labels);
                }
                comparison_results = Some(list);
            } else {
                let by_key: HashMap<&Key, &Vec<Option<f64>>> = compared.iter().map(|row| (&row.dimensions, &row.metrics)).collect();
                let empty: Vec<Option<f64>> = query.metrics.iter().map(|m| empty_value(*m)).collect();
                for (row, out) in results.iter().zip(result_rows.iter_mut()) {
                    let metrics = by_key.get(&row.dimensions).copied().unwrap_or(&empty);
                    out.comparison = Some(ComparedRow {
                        dimensions: row.dimensions.clone(),
                        metrics: numbers(metrics),
                        change: Some(changes(&query.metrics, metrics, &row.metrics)),
                    });
                }
            }
        }

        if let (Some(interval), true) = (interval, query.include.time_labels) {
            meta.time_label_result_indices = Some(result_indices(&labels, &results));
            if query.include.present_index {
                meta.present_index = Some(present_index(&buckets, interval, range, tz, now));
            }
            if query.include.partial_time_labels {
                meta.partial_time_labels = Some(partial_buckets(&buckets, interval, range, tz, now).into_iter().map(|b| interval.label(b)).collect());
            }
            meta.time_labels = Some(labels);
        }

        Ok(QueryResult {
            results: result_rows,
            comparison_results,
            meta,
            query: echo(host, query, range, comparison, &order_by, tz, timezone),
        })
    }

    /// Visitors seen on a site in the last five minutes.
    pub fn current_visitors(&self, host: &QueryHost) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(DISTINCT user_id) FROM events WHERE host_id = ? AND timestamp >= ? AND kind <> ?",
            rusqlite::params![host.id, self.now - 5 * 60, EventKind::Engagement],
            |row| row.get(0),
        )?)
    }

    /// Values of one dimension a filter could match, most visited first.
    pub fn suggestions(&self, host: &QueryHost, dimension: Dimension, mut query: StatsQuery, search: &str, timezone: &str) -> Result<Vec<Suggestion>> {
        let tz = parse_zone(timezone);
        let labelled = match dimension {
            Dimension::Visit(VisitDimension::Country) => Some(VisitDimension::CountryName),
            Dimension::Visit(VisitDimension::Region) => Some(VisitDimension::RegionName),
            Dimension::Visit(VisitDimension::City) => Some(VisitDimension::CityName),
            _ => None,
        };
        query.metrics = vec![Metric::Visitors];
        query.dimensions = std::iter::once(dimension).chain(labelled.map(Dimension::Visit)).collect();
        query.order_by = Some(vec![(Sort::Metric(Metric::Visitors), false)]);
        query.pagination = None;
        query.include = Default::default();

        let ctx = self.context(host, &query, tz)?;
        let range = main_range(&query.date_range, &ctx, None);
        let needle = search.trim().to_lowercase();

        let order = query.order_by.clone().unwrap_or_default();
        let rows = sort_rows(self.rows(&query, &self.scope(host, &query, range, tz, &[]))?, &query, &order);
        Ok(rows
            .into_iter()
            .filter_map(|mut row| {
                let label = row.dimensions.pop().flatten()?;
                let value = row.dimensions.into_iter().next().flatten().unwrap_or_else(|| label.clone());
                Some((value, label))
            })
            .filter(|(value, label)| !value.is_empty() && !label.is_empty())
            .filter(|(_, label)| needle.is_empty() || label.to_lowercase().contains(&needle))
            .take(25)
            .map(|(value, label)| Suggestion { value, label })
            .collect())
    }

    fn context(&self, host: &QueryHost, query: &StatsQuery, tz: Tz) -> Result<PeriodContext> {
        let mut ctx = PeriodContext { tz, now: self.now, relative_date: query.relative_date.unwrap_or_else(|| zoned_date(self.now, tz)), stats_start: None };
        if query.date_range == DateRange::Period(Period::All) {
            let start: Option<i64> = self.conn.query_row("SELECT MIN(start) FROM sessions WHERE host_id = ?", [host.id], |r| r.get(0))?;
            ctx.stats_start = start.map(|start| zoned_date(start, tz));
        }
        Ok(ctx)
    }

    fn scope<'a>(&self, host: &'a QueryHost, query: &'a StatsQuery, range: TimeRange, tz: Tz, buckets: &'a [NaiveDateTime]) -> Scope<'a> {
        Scope { host, range, tz, interval: query.interval(), buckets, dimensions: query.dimensions.clone(), filters: &query.filters }
    }

    /// Every group the query produces over one range, metrics finished.
    fn rows(&self, query: &StatsQuery, scope: &Scope) -> Result<Vec<Row>> {
        let metrics = &query.metrics;
        let pages = scope.dimensions.contains(&Dimension::Page) || scope.filters.iter().any(|f| f.subject == Subject::Page);

        let wants_sessions = metrics.iter().any(|m| from_sessions(*m, pages));
        let wants_events = metrics.iter().any(|m| from_events(*m, pages));
        let has = |metric: Metric| metrics.contains(&metric);

        let events = if wants_events { self.event_groups(scope, has(Metric::TimeOnPage))? } else { Groups::new() };
        let sessions = if wants_sessions { self.session_groups(scope)? } else { Groups::new() };
        let scroll = if has(Metric::ScrollDepth) { self.scroll_groups(scope)? } else { Groups::new() };

        // The visitors whose share each row is, counted the same way as the rows.
        let total = if has(Metric::Percentage) {
            let whole = Scope { dimensions: Vec::new(), ..*scope };
            let groups = if pages { self.event_groups(&whole, false)? } else { self.session_groups(&whole)? };
            Some(groups.get(&Key::new()).and_then(|g| g.visitors).unwrap_or(0.0))
        } else {
            None
        };

        // Exits are visits; exit rate is those over the pageviews of the page
        // left from.
        let pageviews_by_page =
            if has(Metric::ExitRate) { Some(self.event_groups(&Scope { dimensions: vec![Dimension::Page], ..*scope }, false)?) } else { None };

        // A page report is a report of pageviews: an entry page nobody viewed
        // in the range is not a row. Anything else is every group either side
        // saw.
        let mut keys: Vec<&Key> = events.keys().collect();
        if !pages {
            keys.extend(sessions.keys().filter(|k| !events.contains_key(*k)));
        }
        if pages && !wants_events {
            keys.extend(scroll.keys().filter(|k| !events.contains_key(*k)));
        }
        let whole = Key::new();
        if scope.dimensions.is_empty() && keys.is_empty() {
            keys.push(&whole);
        }

        let empty = Aggregates::default();
        let mut rows = Vec::with_capacity(keys.len());

        for key in keys {
            let e = events.get(key).unwrap_or(&empty);
            let s = sessions.get(key).unwrap_or(&empty);
            let side = if pages { e } else { s };

            // Engagement pings are read only for time on page, and a page they
            // alone mention was not viewed in the range.
            if pages && has(Metric::TimeOnPage) && events.contains_key(key) && e.visitors.unwrap_or(0.0) <= 0.0 {
                continue;
            }

            let values = metrics
                .iter()
                .map(|metric| match metric {
                    Metric::Visitors => Some(side.visitors.unwrap_or(0.0)),
                    Metric::Visits => Some(side.visits.unwrap_or(0.0)),
                    Metric::Pageviews => Some(e.pageviews.unwrap_or(0.0)),
                    Metric::ViewsPerVisit => Some(match s.visits.filter(|v| *v != 0.0) {
                        Some(visits) => round(s.pageviews.unwrap_or(0.0) / visits, 2),
                        None => 0.0,
                    }),
                    Metric::BounceRate => Some(match s.bounce_visits.filter(|v| *v != 0.0) {
                        Some(visits) => (s.bounces.unwrap_or(0.0) * 100.0 / visits).round(),
                        None => 0.0,
                    }),
                    Metric::VisitDuration => Some(match s.visits.filter(|v| *v != 0.0) {
                        Some(visits) => (s.duration.unwrap_or(0.0) / visits).round(),
                        None => 0.0,
                    }),
                    Metric::TimeOnPage => e.engaged_visits.filter(|v| *v != 0.0).map(|visits| (e.engaged_time.unwrap_or(0.0) / 1000.0 / visits).round()),
                    Metric::ScrollDepth => {
                        scroll.get(key).and_then(|depth| depth.visits.filter(|v| *v != 0.0).map(|visits| (depth.total.unwrap_or(0.0) / visits).round()))
                    }
                    Metric::Percentage => match total {
                        Some(total) if total != 0.0 => Some(round(side.visitors.unwrap_or(0.0) * 100.0 / total, 2)),
                        _ => None,
                    },
                    Metric::ExitRate => {
                        let page = vec![key.first().cloned().flatten()];
                        let viewed = pageviews_by_page.as_ref().and_then(|g| g.get(&page)).and_then(|g| g.pageviews);
                        viewed.filter(|v| *v != 0.0).map(|viewed| round(s.visits.unwrap_or(0.0) * 100.0 / viewed, 1))
                    }
                })
                .collect();

            let dimensions = key.iter().zip(&scope.dimensions).map(|(value, dimension)| present(*dimension, value)).collect();
            rows.push(Row { dimensions, metrics: values });
        }

        Ok(rows)
    }

    /// Groups over pageviews and engagement, joined to their visits where a
    /// visit's attributes are asked about.
    fn event_groups(&self, scope: &Scope, engagement: bool) -> Result<Groups> {
        let mut params = Params::default();
        let join_visits = scope.joins_visits();

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| event_dimension(*d, scope)).collect();
        let mut conditions = vec![
            format!("e.host_id = {}", params.add(scope.host.id)),
            format!("e.timestamp BETWEEN {} AND {}", params.add(scope.range.first), params.add(scope.range.last)),
        ];
        if join_visits {
            conditions.extend(VISITS_ON.iter().map(|c| c.to_string()));
        }
        let (pageview, engaged) = (EventKind::Pageview as i64, EventKind::Engagement as i64);
        if !engagement {
            conditions.push(format!("e.kind <> {engaged}"));
        }
        for filter in scope.filters {
            conditions.push(filter_sql(filter, &subject_sql(filter.subject), &mut params));
        }

        // Engagement rows are only read for time on page, and everything else
        // has to step around them when they are.
        let counted = |column: &str| if engagement { format!("CASE WHEN e.kind <> {engaged} THEN {column} END") } else { column.to_string() };

        let sql = format!(
            "SELECT {} COUNT(DISTINCT {}) AS visitors, COUNT(DISTINCT {}) AS visits, SUM(e.kind = {pageview}) AS pageviews {}
             FROM events e {} WHERE {} {}",
            select(&dimensions),
            counted("e.user_id"),
            counted("e.session_id"),
            if engagement {
                format!(
                    ", SUM(CASE WHEN e.kind = {engaged} THEN e.engagement_time ELSE 0 END) AS engaged_time,
                       COUNT(DISTINCT CASE WHEN e.kind = {engaged} THEN e.session_id END) AS engaged_visits"
                )
            } else {
                String::new()
            },
            if join_visits { VISITS_JOIN } else { "" },
            conditions.join(" AND "),
            group_by(dimensions.len()),
        );

        self.groups(&sql, params, dimensions.len())
    }

    /// Groups over visits, spread across time buckets where the interval is an
    /// hour or a minute.
    fn session_groups(&self, scope: &Scope) -> Result<Groups> {
        let mut params = Params::default();
        let smear = matches!(scope.interval, Some(Interval::Hour | Interval::Minute));

        let mut buckets = String::new();
        if let (true, Some(interval)) = (smear, scope.interval) {
            let spans = bucket_spans(scope.buckets, interval, scope.tz);
            if spans.is_empty() {
                return Ok(Groups::new());
            }
            let values: Vec<String> =
                spans.into_iter().map(|(label, lo, hi)| format!("({}, {}, {})", params.add(label), params.add(lo), params.add(hi))).collect();
            buckets = format!("WITH buckets (label, lo, hi) AS (VALUES {})", values.join(", "));
        }

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| session_dimension(*d, scope)).collect();
        let page_filters: Vec<&Filter> = scope.filters.iter().filter(|f| f.subject == Subject::Page).collect();

        // A page filter narrows visits to those that saw the page, and the
        // bounce rate to those that landed on it. Written out in the order the
        // statement reads, since that is the order its parameters bind in.
        let bounced = |params: &mut Params| {
            if page_filters.is_empty() {
                "1".to_string()
            } else {
                page_filters.iter().map(|f| filter_sql(f, "s.entry_page", params)).collect::<Vec<_>>().join(" AND ")
            }
        };
        let bounces = bounced(&mut params);
        let bounce_visits = bounced(&mut params);

        let mut conditions = vec![
            format!("s.host_id = {}", params.add(scope.host.id)),
            format!("s.start BETWEEN {} AND {}", params.add(scope.range.first - LONGEST_VISIT), params.add(scope.range.last)),
            format!("s.timestamp >= {}", params.add(scope.range.first)),
        ];
        for filter in scope.filters.iter().filter(|f| f.subject != Subject::Page) {
            conditions.push(filter_sql(filter, &subject_sql(filter.subject), &mut params));
        }
        if !page_filters.is_empty() {
            let host = params.add(scope.host.id);
            let (first, last) = (params.add(scope.range.first), params.add(scope.range.last));
            let pages = page_filters.iter().map(|f| filter_sql(f, "e.pathname", &mut params)).collect::<Vec<_>>().join(" AND ");
            conditions.push(format!(
                "s.session_id IN (SELECT e.session_id FROM events e WHERE e.host_id = {host}
                   AND e.timestamp BETWEEN {first} AND {last} AND e.kind <> {} AND {pages})",
                EventKind::Engagement as i64
            ));
        }

        let sql = format!(
            "{buckets}
             SELECT {} COUNT(DISTINCT s.user_id) AS visitors, COUNT(*) AS visits, SUM(s.pageviews) AS pageviews,
               SUM(CASE WHEN {bounces} THEN s.is_bounce ELSE 0 END) AS bounces,
               SUM(CASE WHEN {bounce_visits} THEN 1 ELSE 0 END) AS bounce_visits,
               SUM(s.duration) AS duration
             FROM sessions s {} WHERE {} {}",
            select(&dimensions),
            if smear { "JOIN buckets b ON s.start < b.hi AND s.timestamp >= b.lo" } else { "" },
            conditions.join(" AND "),
            group_by(dimensions.len()),
        );

        self.groups(&sql, params, dimensions.len())
    }

    /// Each group's average of the deepest each visit scrolled.
    fn scroll_groups(&self, scope: &Scope) -> Result<Groups> {
        let mut params = Params::default();
        let join_visits = scope.joins_visits();

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| event_dimension(*d, scope)).collect();
        let mut conditions = vec![
            format!("e.host_id = {}", params.add(scope.host.id)),
            format!("e.timestamp BETWEEN {} AND {}", params.add(scope.range.first), params.add(scope.range.last)),
        ];
        if join_visits {
            conditions.extend(VISITS_ON.iter().map(|c| c.to_string()));
        }
        conditions.push(format!("e.kind = {}", EventKind::Engagement as i64));
        conditions.push("e.scroll_depth <= 100".into());
        for filter in scope.filters {
            conditions.push(filter_sql(filter, &subject_sql(filter.subject), &mut params));
        }

        let inner: Vec<String> = dimensions.iter().enumerate().map(|(i, e)| format!("{e} AS d{i}")).collect();
        let outer: Vec<String> = (0..dimensions.len()).map(|i| format!("d{i}")).collect();
        let mut inner_group = outer.clone();
        inner_group.push("e.session_id".into());

        let sql = format!(
            "SELECT {} SUM(depth) AS total, COUNT(*) AS visits FROM (
               SELECT {} MAX(e.scroll_depth) AS depth FROM events e {} WHERE {} GROUP BY {}
             ) {}",
            if outer.is_empty() { String::new() } else { format!("{},", outer.join(", ")) },
            if inner.is_empty() { String::new() } else { format!("{},", inner.join(", ")) },
            if join_visits { VISITS_JOIN } else { "" },
            conditions.join(" AND "),
            inner_group.join(", "),
            if outer.is_empty() { String::new() } else { format!("GROUP BY {}", outer.join(", ")) },
        );

        self.groups(&sql, params, dimensions.len())
    }

    fn groups(&self, sql: &str, params: Params, dimensions: usize) -> Result<Groups> {
        let mut statement = self.conn.prepare(sql)?;
        let names: Vec<String> = statement.column_names().into_iter().map(str::to_string).collect();
        let mut rows = statement.query(rusqlite::params_from_iter(params.0))?;
        let mut groups = Groups::new();

        while let Some(row) = rows.next()? {
            let mut key = Key::with_capacity(dimensions);
            let mut aggregates = Aggregates::default();
            for (index, name) in names.iter().enumerate() {
                let value: SqlValue = row.get(index)?;
                if index < dimensions {
                    key.push(match value {
                        SqlValue::Text(text) => Some(text),
                        SqlValue::Integer(i) => Some(i.to_string()),
                        SqlValue::Real(f) => Some(f.to_string()),
                        SqlValue::Null | SqlValue::Blob(_) => None,
                    });
                } else if let Some(column) = aggregates.column(name) {
                    *column = match value {
                        SqlValue::Integer(i) => Some(i as f64),
                        SqlValue::Real(f) => Some(f),
                        _ => None,
                    };
                }
            }
            groups.insert(key, aggregates);
        }

        Ok(groups)
    }
}

fn event_dimension(dimension: Dimension, scope: &Scope) -> String {
    match dimension {
        Dimension::Page => "e.pathname".into(),
        Dimension::Time(_) => time_bucket("e.timestamp", scope),
        Dimension::Visit(visit) => visit_sql(visit),
    }
}

fn session_dimension(dimension: Dimension, scope: &Scope) -> String {
    match dimension {
        // A page broken down by visit-level metrics is the page visits entered on.
        Dimension::Page => "s.entry_page".into(),
        Dimension::Time(Interval::Hour | Interval::Minute) => "b.label".into(),
        // By when the visit was last active, within the range.
        Dimension::Time(_) => time_bucket(&format!("MIN(s.timestamp, {})", scope.range.last), scope),
        Dimension::Visit(visit) => visit_sql(visit),
    }
}

/// A range's buckets, refusing one that would have too many. Counted from the
/// shortest an interval can be, so this errs towards refusing.
fn bounded_buckets(interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Result<Vec<NaiveDateTime>> {
    let shortest = match interval {
        Interval::Minute => 60,
        Interval::Hour => 3600,
        Interval::Day => 86_400,
        Interval::Week => 7 * 86_400,
        Interval::Month => 28 * 86_400,
    };
    if (range.last - range.first) / shortest > MAX_TIME_BUCKETS {
        return Err(QueryError(format!("That range is too long to show by the {}. Pick a shorter range or a longer interval.", interval.name())).into());
    }
    Ok(time_buckets(interval, range, tz, now))
}

fn bucket_labels(interval: Option<Interval>, buckets: &[NaiveDateTime]) -> Vec<String> {
    interval.map_or_else(Vec::new, |interval| buckets.iter().map(|b| interval.label(*b)).collect())
}

/// The SQL for the bucket an instant falls in, as its label. SQLite has no
/// time zones, so the instant is first moved onto local wall time by the
/// zone's offset — a `CASE` over the offsets in force across the range, since
/// a range can straddle a daylight-saving change.
fn time_bucket(column: &str, scope: &Scope) -> String {
    let segments = offset_segments(scope.range.first - LONGEST_VISIT, scope.range.last + LONGEST_VISIT, scope.tz);
    let offset = match segments.as_slice() {
        [(_, only)] => only.to_string(),
        _ => {
            let whens: Vec<String> = segments.windows(2).map(|pair| format!("WHEN {column} < {} THEN {}", pair[1].0, pair[0].1)).collect();
            format!("CASE {} ELSE {} END", whens.join(" "), segments.last().expect("segments").1)
        }
    };
    let local = format!("({column} + {offset})");

    match scope.interval {
        Some(Interval::Minute) => format!("strftime('%Y-%m-%d %H:%M:00', {local}, 'unixepoch')"),
        Some(Interval::Hour) => format!("strftime('%Y-%m-%d %H:00:00', {local}, 'unixepoch')"),
        Some(Interval::Day) => format!("date({local}, 'unixepoch')"),
        Some(Interval::Week) => {
            // Monday of the week, or the range's first day if that is later.
            let (first, _) = date_range(scope.range, scope.tz);
            format!("MAX(date({local}, 'unixepoch', '-6 days', 'weekday 1'), '{first}')")
        }
        Some(Interval::Month) | None => format!("strftime('%Y-%m-01', {local}, 'unixepoch')"),
    }
}

/// A filter as a condition on the expression it applies to.
fn filter_sql(filter: &Filter, expression: &str, params: &mut Params) -> String {
    let lower = !filter.case_sensitive;
    let subject = if lower { format!("LOWER({expression})") } else { expression.to_string() };
    let clauses: Vec<&str> = filter.clauses.iter().map(|c| clause_value(filter.subject, c)).collect();
    let mut value = |clause: &str| params.add(if lower { clause.to_lowercase() } else { clause.to_string() });

    let condition = if filter.operator.exact() {
        format!("{subject} IN ({})", clauses.iter().map(|c| value(c)).collect::<Vec<_>>().join(", "))
    } else {
        format!("({})", clauses.iter().map(|c| format!("INSTR({subject}, {}) > 0", value(c))).collect::<Vec<_>>().join(" OR "))
    };

    if filter.operator.negated() {
        format!("NOT {condition}")
    } else {
        condition
    }
}

/// A filter value as it is stored. Plausible's dashboard spells "no country"
/// as two NUL bytes, and "no city" as the GeoNames id 0.
fn clause_value(subject: Subject, clause: &str) -> &str {
    match (subject, clause) {
        (Subject::Visit(VisitDimension::Country), "\0\0" | "ZZ") | (Subject::Visit(VisitDimension::City), "0") => "",
        _ => clause,
    }
}

/// A dimension value as the response carries it.
fn present(dimension: Dimension, value: &Option<String>) -> Option<String> {
    match (dimension, value) {
        (Dimension::Visit(VisitDimension::CountryName), Some(code)) if !code.is_empty() => {
            Some(country_names().get(code).cloned().unwrap_or_else(|| code.clone()))
        }
        _ => value.clone(),
    }
}

fn select(dimensions: &[String]) -> String {
    dimensions.iter().enumerate().map(|(i, e)| format!("{e} AS d{i},")).collect::<Vec<_>>().join(" ")
}

fn group_by(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        format!("GROUP BY {}", (0..count).map(|i| format!("d{i}")).collect::<Vec<_>>().join(", "))
    }
}

/// `value` rounded to `places` decimals, halves away from zero.
fn round(value: f64, places: i32) -> f64 {
    let factor = 10f64.powi(places);
    (value * factor).round() / factor
}

/// Plausible's order when a query names none: by time, then by the first
/// metric, largest first.
fn default_order(query: &StatsQuery) -> Vec<Order> {
    let by_metric = (Sort::Metric(query.metrics[0]), false);
    match query.dimensions.iter().find(|d| matches!(d, Dimension::Time(_))) {
        Some(time) => vec![(Sort::Dimension(*time), true), by_metric],
        None => vec![by_metric],
    }
}

fn sort_rows(mut rows: Vec<Row>, query: &StatsQuery, order_by: &[Order]) -> Vec<Row> {
    enum Column {
        Metric(usize),
        Dimension(usize),
    }
    let columns: Vec<(Option<Column>, bool)> = order_by
        .iter()
        .map(|(sort, ascending)| {
            let column = match sort {
                Sort::Metric(metric) => query.metrics.iter().position(|m| m == metric).map(Column::Metric),
                Sort::Dimension(dimension) => query.dimensions.iter().position(|d| d == dimension).map(Column::Dimension),
            };
            (column, *ascending)
        })
        .collect();

    rows.sort_by(|a, b| {
        for (column, ascending) in &columns {
            let order = match column {
                Some(Column::Metric(i)) => compare_numbers(a.metrics[*i], b.metrics[*i]),
                // Nulls sort below everything, and text bytewise, which is how
                // Plausible's ClickHouse orders them.
                Some(Column::Dimension(i)) => a.dimensions[*i].cmp(&b.dimensions[*i]),
                None => Ordering::Equal,
            };
            if order != Ordering::Equal {
                return if *ascending { order } else { order.reverse() };
            }
        }
        Ordering::Equal
    });
    rows
}

/// Absent numbers sort below everything.
fn compare_numbers(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(a), Some(b)) => a.total_cmp(&b),
    }
}

/// How each metric changed, Plausible's way: bounce rate by points, exit rate
/// by points to one decimal, everything else by percent.
fn changes(metrics: &[Metric], before: &[Option<f64>], after: &[Option<f64>]) -> Vec<Option<Number>> {
    metrics
        .iter()
        .enumerate()
        .map(|(i, metric)| {
            let (Some(old), Some(new)) = (before[i], after[i]) else {
                return None;
            };
            let change = match metric {
                Metric::BounceRate => (old > 0.0).then_some(new - old)?,
                Metric::ExitRate => round(new - old, 1),
                _ if old == 0.0 => {
                    if new > 0.0 {
                        100.0
                    } else {
                        0.0
                    }
                }
                _ => ((new - old) / old * 100.0).round(),
            };
            Some(Number(change))
        })
        .collect()
}

/// For each label, the row that bucket's answer is in, if any.
fn result_indices(labels: &[String], rows: &[Row]) -> Vec<Option<usize>> {
    let index: HashMap<&str, usize> = rows.iter().enumerate().filter_map(|(i, row)| Some((row.dimensions[0].as_deref()?, i))).collect();
    labels.iter().map(|label| index.get(label.as_str()).copied()).collect()
}

fn echo(host: &QueryHost, query: &StatsQuery, range: TimeRange, comparison: Option<TimeRange>, order_by: &[Order], tz: Tz, timezone: &str) -> Echo {
    let i = &query.include;
    let iso = |range: TimeRange| [iso_in_zone(range.first, tz, timezone), iso_in_zone(range.last, tz, timezone)];
    Echo {
        site_id: host.hostname.clone(),
        metrics: query.metrics.iter().map(|m| m.name()).collect(),
        date_range: iso(range),
        comparison_date_range: comparison.map(iso),
        filters: query.raw_filters.clone(),
        dimensions: query.dimensions.iter().map(|d| d.name()).collect(),
        order_by: order_by.iter().map(|(sort, ascending)| [sort.name(), if *ascending { "asc" } else { "desc" }.into()]).collect(),
        include: EchoInclude {
            imports: i.imports,
            imports_meta: i.imports_meta,
            time_labels: i.time_labels,
            time_label_result_indices: i.time_labels,
            partial_time_labels: i.partial_time_labels,
            compare: i.compare.is_some().then(|| i.compare_raw.clone()),
            compare_match_day_of_week: i.compare_match_day_of_week,
            empty_metrics: i.empty_metrics,
            present_index: i.present_index,
            trim_relative_date_range: true,
        },
        pagination: query.pagination.map(|(limit, offset)| Pagination { limit, offset }),
    }
}
