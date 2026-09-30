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

use chrono_tz::Tz;
use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use super::parse::{Filter, QueryError, StatsQuery};
use super::period::*;
use super::zone::{iso_in_zone, offset_segments, parse_zone, zoned_date};
use crate::util::{js_number, js_round};

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

#[derive(Clone, Debug)]
pub struct Row {
    pub dimensions: Vec<Value>,
    pub metrics: Vec<Option<f64>>,
}

/// What one grouped query returns per group, before metrics are finished.
type Group = HashMap<String, Option<f64>>;
type Groups = IndexMap<String, (Vec<Value>, Group)>;

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
fn visit_sql(dimension: &str) -> String {
    match dimension {
        "visit:source" => not_set("s.source", "Direct / None"),
        "visit:referrer" => not_set("s.referrer", "Direct / None"),
        "visit:channel" => not_set("s.channel", "Direct"),
        "visit:utm_medium" => not_set("s.utm_medium", "(not set)"),
        "visit:utm_source" => not_set("s.utm_source", "(not set)"),
        "visit:utm_campaign" => not_set("s.utm_campaign", "(not set)"),
        "visit:utm_content" => not_set("s.utm_content", "(not set)"),
        "visit:utm_term" => not_set("s.utm_term", "(not set)"),
        "visit:country" | "visit:country_name" => "s.country".into(),
        // Regions and cities are known by name here rather than by ISO code
        // or GeoNames id, which DB-IP's free database doesn't carry.
        "visit:region" => "CASE WHEN s.region = '' THEN '' ELSE s.country || '-' || s.region END".into(),
        "visit:region_name" => "s.region".into(),
        "visit:city" => "CASE WHEN s.city = '' THEN '' ELSE s.country || '-' || s.region || '-' || s.city END".into(),
        "visit:city_name" => "s.city".into(),
        "visit:device" => not_set("s.device", "(not set)"),
        "visit:browser" => not_set("s.browser", "(not set)"),
        "visit:browser_version" => not_set("s.browser_version", "(not set)"),
        "visit:os" => not_set("s.os", "(not set)"),
        "visit:os_version" => not_set("s.os_version", "(not set)"),
        "visit:entry_page" => "s.entry_page".into(),
        "visit:exit_page" => "s.exit_page".into(),
        _ => "''".into(),
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
    labels: Vec<String>,
    dimensions: Vec<String>,
    filters: &'a [Filter],
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
    pub fn query(&self, host: &QueryHost, query: &StatsQuery, timezone: &str) -> Result<Value> {
        let tz = parse_zone(timezone);
        let now = self.now;
        let ctx = self.context(host, query, tz)?;
        let interval = query.interval();
        let compare = query.include.compare.as_ref();

        let range = main_range(&query.date_range, &ctx, compare);
        let labels = match interval {
            Some(interval) => bounded_labels(interval, range, tz, now)?,
            None => Vec::new(),
        };
        let order_by = query.order_by.clone().unwrap_or_else(|| default_order(query));

        let all = sort_rows(self.rows(query, &self.scope(host, query, range, tz, labels.clone()))?, query, &order_by);
        let (limit, offset) = query.pagination.unwrap_or((10_000, 0));
        let results: Vec<Row> = all.into_iter().skip(offset as usize).take(limit as usize).collect();
        let mut result_json: Vec<Value> = results.iter().map(row_json).collect();

        let mut meta = Map::new();
        if query.include.imports || query.include.imports_meta {
            meta.insert("imports_included".into(), Value::Bool(false));
        }
        if query.include.empty_metrics {
            meta.insert("empty_metrics".into(), query.metrics.iter().map(|m| metric_json(empty_value(m))).collect());
        }

        let mut comparison = None;
        let mut comparison_results = None;

        if let Some(compare) = compare {
            let comparison_range = comparison_range(&query.date_range, range, compare, query.include.compare_match_day_of_week, &ctx);
            comparison = Some(comparison_range);
            let comparison_labels = match interval {
                Some(interval) => bounded_labels(interval, comparison_range, tz, now)?,
                None => Vec::new(),
            };
            let compared = self.rows(query, &self.scope(host, query, comparison_range, tz, comparison_labels.clone()))?;

            if let (Some(interval), 1) = (interval, query.dimensions.len()) {
                // A time series: two lists, each only its non-empty buckets,
                // with each comparison bucket's change measured against the
                // bucket it lines up with.
                let by_label: HashMap<String, &Vec<Option<f64>>> =
                    results.iter().map(|row| (value_string(&row.dimensions[0]), &row.metrics)).collect();
                let line_up: HashMap<&String, &String> = comparison_labels.iter().zip(labels.iter()).collect();

                let sorted = sort_rows(compared, query, &order_by);
                let list: Vec<Value> = sorted
                    .iter()
                    .map(|row| {
                        let main = line_up.get(&value_string(&row.dimensions[0])).and_then(|label| by_label.get(*label));
                        let mut object = row_object(row);
                        object.insert(
                            "change".into(),
                            main.map_or(Value::Null, |main| changes_json(&query.metrics, &row.metrics, main)),
                        );
                        Value::Object(object)
                    })
                    .collect();

                if query.include.time_labels {
                    meta.insert("comparison_time_labels".into(), json!(comparison_labels));
                    meta.insert("comparison_time_label_result_indices".into(), result_indices(&comparison_labels, &sorted));
                    if query.include.partial_time_labels {
                        meta.insert(
                            "comparison_partial_time_labels".into(),
                            json!(partial_time_labels(&comparison_labels, interval, comparison_range, tz, now)),
                        );
                    }
                }
                comparison_results = Some(list);
            } else {
                let by_key: HashMap<String, &Vec<Option<f64>>> = compared.iter().map(|row| (key(&row.dimensions), &row.metrics)).collect();
                for (row, out) in results.iter().zip(result_json.iter_mut()) {
                    let empty: Vec<Option<f64>> = query.metrics.iter().map(|m| empty_value(m)).collect();
                    let metrics = by_key.get(&key(&row.dimensions)).copied().unwrap_or(&empty);
                    out["comparison"] = json!({
                        "dimensions": row.dimensions,
                        "metrics": metrics.iter().map(|m| metric_json(*m)).collect::<Vec<_>>(),
                        "change": changes_json(&query.metrics, metrics, &row.metrics),
                    });
                }
            }
        }

        if let (Some(interval), true) = (interval, query.include.time_labels) {
            meta.insert("time_labels".into(), json!(labels));
            meta.insert("time_label_result_indices".into(), result_indices(&labels, &results));
            if query.include.present_index {
                meta.insert("present_index".into(), json!(present_index(&labels, interval, range, tz, now)));
            }
            if query.include.partial_time_labels {
                meta.insert("partial_time_labels".into(), json!(partial_time_labels(&labels, interval, range, tz, now)));
            }
        }

        let mut response = Map::new();
        response.insert("results".into(), Value::Array(result_json));
        if let Some(list) = comparison_results {
            response.insert("comparison_results".into(), Value::Array(list));
        }
        meta.sort_keys();
        response.insert("meta".into(), Value::Object(meta));
        response.insert("query".into(), echo(host, query, range, comparison, &order_by, tz, timezone));

        Ok(Value::Object(response))
    }

    /// Visitors seen on a site in the last five minutes.
    pub fn current_visitors(&self, host: &QueryHost) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(DISTINCT user_id) FROM events WHERE host_id = ? AND timestamp >= ? AND kind <> 1",
            rusqlite::params![host.id, self.now - 5 * 60],
            |row| row.get(0),
        )?)
    }

    /// Values of one dimension a filter could match, most visited first.
    pub fn suggestions(&self, host: &QueryHost, dimension: &str, mut query: StatsQuery, search: &str, timezone: &str) -> Result<Value> {
        let tz = parse_zone(timezone);
        let labelled = match dimension {
            "visit:country" => Some("visit:country_name"),
            "visit:region" => Some("visit:region_name"),
            "visit:city" => Some("visit:city_name"),
            _ => None,
        };
        query.metrics = vec!["visitors".into()];
        query.dimensions = std::iter::once(dimension.to_string()).chain(labelled.map(str::to_string)).collect();
        query.order_by = Some(vec![("visitors".into(), "desc".into())]);
        query.pagination = None;
        query.include = Default::default();

        let ctx = self.context(host, &query, tz)?;
        let range = main_range(&query.date_range, &ctx, None);
        let needle = search.trim().to_lowercase();

        let rows = sort_rows(self.rows(&query, &self.scope(host, &query, range, tz, Vec::new()))?, &query, query.order_by.as_ref().unwrap());
        let options: Vec<Value> = rows
            .iter()
            .map(|row| (value_string(&row.dimensions[0]), value_string(row.dimensions.last().unwrap_or(&Value::Null))))
            .filter(|(value, label)| !value.is_empty() && !label.is_empty())
            .filter(|(_, label)| needle.is_empty() || label.to_lowercase().contains(&needle))
            .take(25)
            .map(|(value, label)| json!({ "value": value, "label": label }))
            .collect();
        Ok(Value::Array(options))
    }

    fn context(&self, host: &QueryHost, query: &StatsQuery, tz: Tz) -> Result<PeriodContext> {
        let mut ctx = PeriodContext {
            tz,
            now: self.now,
            relative_date: query.relative_date.clone().unwrap_or_else(|| zoned_date(self.now, tz)),
            stats_start: None,
        };
        if query.date_range == DateRange::Period("all".into()) {
            let start: Option<i64> = self.conn.query_row("SELECT MIN(start) FROM sessions WHERE host_id = ?", [host.id], |r| r.get(0))?;
            ctx.stats_start = start.map(|start| zoned_date(start, tz));
        }
        Ok(ctx)
    }

    fn scope<'a>(&self, host: &'a QueryHost, query: &'a StatsQuery, range: TimeRange, tz: Tz, labels: Vec<String>) -> Scope<'a> {
        Scope { host, range, tz, interval: query.interval(), labels, dimensions: query.dimensions.clone(), filters: &query.filters }
    }

    /// Every group the query produces over one range, metrics finished.
    fn rows(&self, query: &StatsQuery, scope: &Scope) -> Result<Vec<Row>> {
        let metrics = &query.metrics;
        let pages = scope.dimensions.iter().any(|d| d == "event:page") || scope.filters.iter().any(|f| f.dimension == "event:page");

        // Which side each metric is counted on. Visitors and visits follow the
        // question: pageviews when it is about pages, visits otherwise.
        let session_metrics: &[&str] = if pages {
            &["bounce_rate", "visit_duration", "views_per_visit"]
        } else {
            &["visitors", "visits", "percentage", "bounce_rate", "visit_duration", "views_per_visit", "exit_rate"]
        };
        let event_metrics: &[&str] =
            if pages { &["visitors", "visits", "percentage", "pageviews", "time_on_page"] } else { &["pageviews", "time_on_page"] };

        let wants = |set: &[&str]| metrics.iter().any(|m| set.contains(&m.as_str()));
        let wants_sessions = wants(session_metrics);
        let wants_events = wants(event_metrics);
        let wants_scroll = metrics.iter().any(|m| m == "scroll_depth");
        let has = |m: &str| metrics.iter().any(|x| x == m);

        let events = if wants_events { self.event_groups(scope, has("time_on_page"))? } else { Groups::new() };
        let sessions = if wants_sessions { self.session_groups(scope)? } else { Groups::new() };
        let scroll = if wants_scroll { self.scroll_groups(scope)? } else { Groups::new() };

        // The visitors whose share each row is, counted the same way as the rows.
        let total = if has("percentage") {
            let whole = Scope { dimensions: Vec::new(), labels: scope.labels.clone(), ..*scope };
            let groups = if pages { self.event_groups(&whole, false)? } else { self.session_groups(&whole)? };
            Some(groups.get(&key(&[])).and_then(|(_, g)| g.get("visitors").copied().flatten()).unwrap_or(0.0))
        } else {
            None
        };

        // Exits are visits; exit rate is those over the pageviews of the page
        // left from.
        let pageviews_by_page = if has("exit_rate") {
            Some(self.event_groups(&Scope { dimensions: vec!["event:page".into()], labels: scope.labels.clone(), ..*scope }, false)?)
        } else {
            None
        };

        // A page report is a report of pageviews: an entry page nobody viewed
        // in the range is not a row. Anything else is every group either side
        // saw.
        let mut keys: IndexMap<String, Vec<Value>> = IndexMap::new();
        for (k, (dims, _)) in events.iter() {
            keys.entry(k.clone()).or_insert_with(|| dims.clone());
        }
        if !pages {
            for (k, (dims, _)) in sessions.iter() {
                keys.entry(k.clone()).or_insert_with(|| dims.clone());
            }
        }
        if pages && !wants_events {
            for (k, (dims, _)) in scroll.iter() {
                keys.entry(k.clone()).or_insert_with(|| dims.clone());
            }
        }
        if scope.dimensions.is_empty() {
            keys.entry(key(&[])).or_default();
        }

        let empty = Group::new();
        let get = |g: &Group, name: &str| g.get(name).copied().flatten();
        let mut rows = Vec::with_capacity(keys.len());

        for (k, dims) in &keys {
            let e = events.get(k).map(|(_, g)| g).unwrap_or(&empty);
            let s = sessions.get(k).map(|(_, g)| g).unwrap_or(&empty);
            let side = if pages { e } else { s };

            // Engagement pings are read only for time on page, and a page they
            // alone mention was not viewed in the range.
            if pages && has("time_on_page") && events.contains_key(k) && get(e, "visitors").unwrap_or(0.0) <= 0.0 {
                continue;
            }

            let values = metrics
                .iter()
                .map(|metric| match metric.as_str() {
                    "visitors" => Some(get(side, "visitors").unwrap_or(0.0)),
                    "visits" => Some(get(side, "visits").unwrap_or(0.0)),
                    "pageviews" => Some(get(e, "pageviews").unwrap_or(0.0)),
                    "views_per_visit" => Some(match get(s, "visits").filter(|v| *v != 0.0) {
                        Some(visits) => round(get(s, "pageviews").unwrap_or(0.0) / visits, 2),
                        None => 0.0,
                    }),
                    "bounce_rate" => Some(match get(s, "bounce_visits").filter(|v| *v != 0.0) {
                        Some(visits) => js_round(get(s, "bounces").unwrap_or(0.0) * 100.0 / visits),
                        None => 0.0,
                    }),
                    "visit_duration" => Some(match get(s, "visits").filter(|v| *v != 0.0) {
                        Some(visits) => js_round(get(s, "duration").unwrap_or(0.0) / visits),
                        None => 0.0,
                    }),
                    "time_on_page" => get(e, "engaged_visits")
                        .filter(|v| *v != 0.0)
                        .map(|visits| js_round(get(e, "engaged_time").unwrap_or(0.0) / 1000.0 / visits)),
                    "scroll_depth" => scroll.get(k).and_then(|(_, depth)| {
                        get(depth, "visits").filter(|v| *v != 0.0).map(|visits| js_round(get(depth, "total").unwrap_or(0.0) / visits))
                    }),
                    "percentage" => match total {
                        Some(total) if total != 0.0 => Some(round(get(side, "visitors").unwrap_or(0.0) * 100.0 / total, 2)),
                        _ => None,
                    },
                    "exit_rate" => {
                        let page = key(&[dims.first().cloned().unwrap_or(Value::Null)]);
                        let viewed = pageviews_by_page.as_ref().and_then(|g| g.get(&page)).and_then(|(_, g)| get(g, "pageviews"));
                        viewed.filter(|v| *v != 0.0).map(|viewed| round(get(s, "visits").unwrap_or(0.0) * 100.0 / viewed, 1))
                    }
                    _ => None,
                })
                .collect();

            let dimensions = dims.iter().zip(&scope.dimensions).map(|(value, dimension)| present(dimension, value)).collect();
            rows.push(Row { dimensions, metrics: values });
        }

        Ok(rows)
    }

    /// Groups over pageviews and engagement, joined to their visits where a
    /// visit's attributes are asked about.
    fn event_groups(&self, scope: &Scope, engagement: bool) -> Result<Groups> {
        let mut params = Params::default();
        let join_visits = scope.dimensions.iter().any(|d| d.starts_with("visit:")) || scope.filters.iter().any(|f| f.dimension.starts_with("visit:"));

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| event_dimension(d, scope)).collect();
        let mut conditions = vec![
            format!("e.host_id = {}", params.add(scope.host.id)),
            format!("e.timestamp BETWEEN {} AND {}", params.add(scope.range.first), params.add(scope.range.last)),
        ];
        if join_visits {
            conditions.extend(VISITS_ON.iter().map(|c| c.to_string()));
        }
        if !engagement {
            conditions.push("e.kind <> 1".into());
        }
        for filter in scope.filters {
            let expression = if filter.dimension == "event:page" { "e.pathname".to_string() } else { visit_sql(&filter.dimension) };
            conditions.push(filter_sql(filter, &expression, &mut params));
        }

        // Engagement rows are only read for time on page, and everything else
        // has to step around them when they are.
        let counted = |column: &str| if engagement { format!("CASE WHEN e.kind <> 1 THEN {column} END") } else { column.to_string() };

        let sql = format!(
            "SELECT {} COUNT(DISTINCT {}) AS visitors, COUNT(DISTINCT {}) AS visits, SUM(e.kind = 0) AS pageviews {}
             FROM events e {} WHERE {} {}",
            select(&dimensions),
            counted("e.user_id"),
            counted("e.session_id"),
            if engagement {
                ", SUM(CASE WHEN e.kind = 1 THEN e.engagement_time ELSE 0 END) AS engaged_time,
                   COUNT(DISTINCT CASE WHEN e.kind = 1 THEN e.session_id END) AS engaged_visits"
            } else {
                ""
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
        if smear {
            let spans = bucket_spans(&scope.labels, scope.interval.unwrap(), scope.tz);
            if spans.is_empty() {
                return Ok(Groups::new());
            }
            let values: Vec<String> = spans
                .into_iter()
                .map(|(label, lo, hi)| format!("({}, {}, {})", params.add(label), params.add(lo), params.add(hi)))
                .collect();
            buckets = format!("WITH buckets (label, lo, hi) AS (VALUES {})", values.join(", "));
        }

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| session_dimension(d, scope)).collect();
        let page_filters: Vec<&Filter> = scope.filters.iter().filter(|f| f.dimension == "event:page").collect();

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
        for filter in scope.filters.iter().filter(|f| f.dimension != "event:page") {
            conditions.push(filter_sql(filter, &visit_sql(&filter.dimension), &mut params));
        }
        if !page_filters.is_empty() {
            let host = params.add(scope.host.id);
            let (first, last) = (params.add(scope.range.first), params.add(scope.range.last));
            let pages = page_filters.iter().map(|f| filter_sql(f, "e.pathname", &mut params)).collect::<Vec<_>>().join(" AND ");
            conditions.push(format!(
                "s.session_id IN (SELECT e.session_id FROM events e WHERE e.host_id = {host}
                   AND e.timestamp BETWEEN {first} AND {last} AND e.kind <> 1 AND {pages})"
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
        let join_visits = scope.dimensions.iter().any(|d| d.starts_with("visit:")) || scope.filters.iter().any(|f| f.dimension.starts_with("visit:"));

        let dimensions: Vec<String> = scope.dimensions.iter().map(|d| event_dimension(d, scope)).collect();
        let mut conditions = vec![
            format!("e.host_id = {}", params.add(scope.host.id)),
            format!("e.timestamp BETWEEN {} AND {}", params.add(scope.range.first), params.add(scope.range.last)),
        ];
        if join_visits {
            conditions.extend(VISITS_ON.iter().map(|c| c.to_string()));
        }
        conditions.push("e.kind = 1".into());
        conditions.push("e.scroll_depth <= 100".into());
        for filter in scope.filters {
            let expression = if filter.dimension == "event:page" { "e.pathname".to_string() } else { visit_sql(&filter.dimension) };
            conditions.push(filter_sql(filter, &expression, &mut params));
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
            let mut values = Vec::with_capacity(dimensions);
            let mut metrics = Group::new();
            for (index, name) in names.iter().enumerate() {
                let value: SqlValue = row.get(index)?;
                if index < dimensions {
                    values.push(match value {
                        SqlValue::Null => Value::Null,
                        SqlValue::Integer(i) => Value::from(i),
                        SqlValue::Real(f) => js_number(f),
                        SqlValue::Text(t) => Value::String(t),
                        SqlValue::Blob(_) => Value::Null,
                    });
                } else {
                    metrics.insert(
                        name.clone(),
                        match value {
                            SqlValue::Integer(i) => Some(i as f64),
                            SqlValue::Real(f) => Some(f),
                            SqlValue::Text(t) => t.parse().ok(),
                            _ => None,
                        },
                    );
                }
            }
            groups.insert(key(&values), (values, metrics));
        }

        Ok(groups)
    }
}

fn event_dimension(dimension: &str, scope: &Scope) -> String {
    if dimension == "event:page" {
        "e.pathname".into()
    } else if dimension.starts_with("time:") {
        time_bucket("e.timestamp", scope)
    } else {
        visit_sql(dimension)
    }
}

fn session_dimension(dimension: &str, scope: &Scope) -> String {
    // A page broken down by visit-level metrics is the page visits entered on.
    if dimension == "event:page" {
        "s.entry_page".into()
    } else if dimension.starts_with("time:") {
        if matches!(scope.interval, Some(Interval::Hour | Interval::Minute)) {
            "b.label".into()
        } else {
            // By when the visit was last active, within the range.
            time_bucket(&format!("MIN(s.timestamp, {})", scope.range.last), scope)
        }
    } else {
        visit_sql(dimension)
    }
}

/// Labels for a range, refusing one that would have too many buckets. Counted
/// from the shortest an interval can be, so this errs towards refusing.
fn bounded_labels(interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Result<Vec<String>> {
    let shortest = match interval {
        Interval::Minute => 60,
        Interval::Hour => 3600,
        Interval::Day => 86_400,
        Interval::Week => 7 * 86_400,
        Interval::Month => 28 * 86_400,
    };
    if (range.last - range.first) / shortest > MAX_TIME_BUCKETS {
        return Err(QueryError(format!(
            "That range is too long to show by the {}. Pick a shorter range or a longer interval.",
            interval.name()
        ))
        .into());
    }
    Ok(time_labels(interval, range, tz, now))
}

/// The SQL for the bucket an instant falls in, as its label. SQLite has no
/// time zones, so the instant is first moved onto local wall time by the
/// zone's offset — a `CASE` over the offsets in force across the range, since
/// a range can straddle a daylight-saving change.
fn time_bucket(column: &str, scope: &Scope) -> String {
    let segments = offset_segments(scope.range.first - LONGEST_VISIT, scope.range.last + LONGEST_VISIT, scope.tz);
    let offset = if segments.len() == 1 {
        segments[0].1.to_string()
    } else {
        let whens: Vec<String> =
            segments.windows(2).map(|pair| format!("WHEN {column} < {} THEN {}", pair[1].0, pair[0].1)).collect();
        format!("CASE {} ELSE {} END", whens.join(" "), segments.last().unwrap().1)
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
        _ => format!("strftime('%Y-%m-01', {local}, 'unixepoch')"),
    }
}

/// A filter as a condition on the expression it applies to.
fn filter_sql(filter: &Filter, expression: &str, params: &mut Params) -> String {
    let lower = !filter.case_sensitive;
    let subject = if lower { format!("LOWER({expression})") } else { expression.to_string() };
    let clauses: Vec<String> = filter.clauses.iter().map(|c| clause_value(&filter.dimension, c)).collect();
    let mut value = |clause: &str| params.add(if lower { clause.to_lowercase() } else { clause.to_string() });

    let condition = if filter.operator == "is" || filter.operator == "is_not" {
        format!("{subject} IN ({})", clauses.iter().map(|c| value(c)).collect::<Vec<_>>().join(", "))
    } else {
        format!("({})", clauses.iter().map(|c| format!("INSTR({subject}, {}) > 0", value(c))).collect::<Vec<_>>().join(" OR "))
    };

    if filter.operator == "is" || filter.operator == "contains" {
        condition
    } else {
        format!("NOT {condition}")
    }
}

/// A filter value as it is stored. Plausible's dashboard spells "no country"
/// as two NUL bytes, and "no city" as the GeoNames id 0.
fn clause_value(dimension: &str, clause: &str) -> String {
    if dimension == "visit:country" && (clause == "\0\0" || clause == "ZZ") {
        return String::new();
    }
    if dimension == "visit:city" && clause == "0" {
        return String::new();
    }
    clause.to_string()
}

/// A dimension value as the response carries it.
fn present(dimension: &str, value: &Value) -> Value {
    if dimension == "visit:country_name" {
        if let Value::String(code) = value {
            if !code.is_empty() {
                return Value::String(country_names().get(code).cloned().unwrap_or_else(|| code.clone()));
            }
        }
    }
    value.clone()
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

fn key(values: &[Value]) -> String {
    serde_json::to_string(values).unwrap_or_default()
}

fn value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn round(value: f64, places: i32) -> f64 {
    let factor = 10f64.powi(places);
    js_round(value * factor) / factor
}

fn metric_json(value: Option<f64>) -> Value {
    value.map_or(Value::Null, js_number)
}

fn row_object(row: &Row) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("dimensions".into(), Value::Array(row.dimensions.clone()));
    object.insert("metrics".into(), row.metrics.iter().map(|m| metric_json(*m)).collect());
    object
}

fn row_json(row: &Row) -> Value {
    Value::Object(row_object(row))
}

/// What a metric reads as where there is nothing to measure.
fn empty_value(metric: &str) -> Option<f64> {
    match metric {
        "visit_duration" | "exit_rate" | "scroll_depth" | "time_on_page" => None,
        _ => Some(0.0),
    }
}

/// Plausible's order when a query names none: by time, then by the first
/// metric, largest first.
fn default_order(query: &StatsQuery) -> Vec<(String, String)> {
    let by_metric = (query.metrics[0].clone(), "desc".to_string());
    match query.dimensions.iter().find(|d| d.starts_with("time:")) {
        Some(time) => vec![(time.clone(), "asc".into()), by_metric],
        None => vec![by_metric],
    }
}

fn sort_rows(mut rows: Vec<Row>, query: &StatsQuery, order_by: &[(String, String)]) -> Vec<Row> {
    let positions: Vec<(Option<usize>, Option<usize>, bool)> = order_by
        .iter()
        .map(|(name, direction)| {
            (query.metrics.iter().position(|m| m == name), query.dimensions.iter().position(|d| d == name), direction == "asc")
        })
        .collect();

    rows.sort_by(|a, b| {
        for (metric, dimension, ascending) in &positions {
            let order = match (metric, dimension) {
                (Some(i), _) => compare_numbers(a.metrics[*i], b.metrics[*i]),
                (None, Some(i)) => compare_values(&a.dimensions[*i], &b.dimensions[*i]),
                _ => Ordering::Equal,
            };
            if order != Ordering::Equal {
                return if *ascending { order } else { order.reverse() };
            }
        }
        Ordering::Equal
    });
    rows
}

fn compare_numbers(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

/// Nulls sort below everything, numbers numerically, strings bytewise — which
/// is how Plausible's ClickHouse orders them.
fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Number(a), Value::Number(b)) => a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(Ordering::Equal),
        _ => value_string(a).cmp(&value_string(b)),
    }
}

/// How each metric changed, Plausible's way: bounce rate by points, exit rate
/// by points to one decimal, everything else by percent.
fn changes_json(metrics: &[String], before: &[Option<f64>], after: &[Option<f64>]) -> Value {
    metrics
        .iter()
        .enumerate()
        .map(|(i, metric)| {
            let (Some(old), Some(new)) = (before[i], after[i]) else {
                return Value::Null;
            };
            match metric.as_str() {
                "bounce_rate" => {
                    if old > 0.0 {
                        js_number(new - old)
                    } else {
                        Value::Null
                    }
                }
                "exit_rate" => js_number(round(new - old, 1)),
                _ if old == 0.0 => js_number(if new > 0.0 { 100.0 } else { 0.0 }),
                _ => js_number(js_round((new - old) / old * 100.0)),
            }
        })
        .collect()
}

fn result_indices(labels: &[String], rows: &[Row]) -> Value {
    let index: HashMap<String, usize> = rows.iter().enumerate().map(|(i, row)| (value_string(&row.dimensions[0]), i)).collect();
    labels.iter().map(|label| index.get(label).map_or(Value::Null, |i| Value::from(*i))).collect()
}

/// The query as it was run, in the shape Plausible echoes it back.
fn echo(
    host: &QueryHost,
    query: &StatsQuery,
    range: TimeRange,
    comparison: Option<TimeRange>,
    order_by: &[(String, String)],
    tz: Tz,
    timezone: &str,
) -> Value {
    let i = &query.include;
    let mut include = Map::new();
    let flags = [
        ("imports", i.imports),
        ("imports_meta", i.imports_meta),
        ("time_labels", i.time_labels),
        ("time_label_result_indices", i.time_labels),
        ("partial_time_labels", i.partial_time_labels),
    ];
    for (name, value) in flags {
        if value {
            include.insert(name.into(), Value::Bool(true));
        }
    }
    if i.compare.is_some() {
        include.insert("compare".into(), i.compare_raw.clone());
    }
    for (name, value) in [
        ("compare_match_day_of_week", i.compare_match_day_of_week),
        ("empty_metrics", i.empty_metrics),
        ("present_index", i.present_index),
    ] {
        if value {
            include.insert(name.into(), Value::Bool(true));
        }
    }
    include.insert("trim_relative_date_range".into(), Value::Bool(true));

    let mut echoed = Map::new();
    echoed.insert("site_id".into(), json!(host.hostname));
    echoed.insert("metrics".into(), json!(query.metrics));
    echoed.insert("date_range".into(), json!([iso_in_zone(range.first, tz, timezone), iso_in_zone(range.last, tz, timezone)]));
    if let Some(comparison) = comparison {
        echoed.insert(
            "comparison_date_range".into(),
            json!([iso_in_zone(comparison.first, tz, timezone), iso_in_zone(comparison.last, tz, timezone)]),
        );
    }
    echoed.insert("filters".into(), Value::Array(query.raw_filters.clone()));
    echoed.insert("dimensions".into(), json!(query.dimensions));
    echoed.insert("order_by".into(), order_by.iter().map(|(a, b)| json!([a, b])).collect());
    echoed.insert("include".into(), Value::Object(include));
    if let Some((limit, offset)) = query.pagination {
        echoed.insert("pagination".into(), json!({ "limit": limit, "offset": offset }));
    }
    Value::Object(echoed)
}
