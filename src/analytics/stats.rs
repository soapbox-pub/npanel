//! The dashboard's questions — a query, the live visitor count, filter
//! suggestions, and the list of sites' sparklines — answered as the HTTP
//! responses Plausible's endpoints give.

use std::collections::HashMap;

use http::StatusCode;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use super::query::engine::{Number, QueryEngine, QueryHost, StatsError};
use super::query::parse::{parse_stats_query, Dimension, Metric, QueryError, StatsQuery, VisitDimension};
use super::query::period::{Compare, Interval, Period};
use super::Answer;
use crate::fields;
use crate::log::Logger;

/// The suggestions endpoint's filter names, and the dimension each one means.
fn suggestion_dimension(filter: &str) -> Option<Dimension> {
    use VisitDimension::*;
    let visit = match filter {
        "page" => return Some(Dimension::Page),
        "entry_page" => EntryPage,
        "exit_page" => ExitPage,
        "source" => Source,
        "channel" => Channel,
        "referrer" => Referrer,
        "utm_medium" => UtmMedium,
        "utm_source" => UtmSource,
        "utm_campaign" => UtmCampaign,
        "utm_content" => UtmContent,
        "utm_term" => UtmTerm,
        "country" => Country,
        "region" => Region,
        "city" => City,
        "screen" => Device,
        "browser" => Browser,
        "browser_version" => BrowserVersion,
        "os" => Os,
        "os_version" => OsVersion,
        _ => return None,
    };
    Some(Dimension::Visit(visit))
}

pub fn query(conn: &Connection, log: &Logger, now: i64, host: &QueryHost, body: &Value, timezone: &str) -> Answer {
    answer(log, || {
        let query = parse_stats_query(body)?;
        QueryEngine::new(conn, now).query(host, &query, timezone)
    })
}

pub fn current_visitors(conn: &Connection, log: &Logger, now: i64, host: &QueryHost) -> Answer {
    answer(log, || QueryEngine::new(conn, now).current_visitors(host))
}

/// One site's last 24 hours.
#[derive(Serialize)]
struct Sparkline {
    visitors: Number,
    /// Against the 24 hours before.
    change: Option<Number>,
    /// The visitors of each hour.
    hours: Vec<Option<Number>>,
}

/// The last 24 hours of each site: unique visitors, their change against the
/// 24 hours before, and the visitors of each hour. Two queries a site, since
/// hourly buckets don't add up to a day's unique visitors.
pub fn sparklines(conn: &Connection, log: &Logger, now: i64, hosts: &[QueryHost], timezone: &str) -> Answer {
    answer(log, || {
        let mut total = StatsQuery::new(Period::Hours24, vec![Metric::Visitors]);
        total.include.compare = Some(Compare::PreviousPeriod);
        let mut series = StatsQuery::new(Period::Hours24, vec![Metric::Visitors]);
        series.dimensions = vec![Dimension::Time(Interval::Hour)];
        series.include.time_labels = true;

        let engine = QueryEngine::new(conn, now);
        let mut sparklines = serde_json::Map::new();
        for host in hosts {
            let total = engine.query(host, &total, timezone)?;
            let hourly = engine.query(host, &series, timezone)?;

            let row = total.results.first();
            let hours = hourly
                .meta
                .time_label_result_indices
                .unwrap_or_default()
                .into_iter()
                .map(|index| match index {
                    Some(i) => hourly.results[i].metrics[0],
                    None => Some(Number(0.0)),
                })
                .collect();
            let sparkline = Sparkline {
                visitors: row.and_then(|row| row.metrics[0]).unwrap_or(Number(0.0)),
                change: row.and_then(|row| row.comparison.as_ref()?.change.as_ref()?[0]),
                hours,
            };
            sparklines.insert(host.hostname.clone(), json!(sparkline));
        }
        Ok(json!({ "sparklines": sparklines }))
    })
}

/// Suggestions take the dashboard's older query-string form: `period`,
/// `date`, `from` and `to` for the range, `filters` as a JSON list, and `q`
/// for what has been typed.
pub fn suggestions(
    conn: &Connection,
    log: &Logger,
    now: i64,
    host: &QueryHost,
    filter: &str,
    params: &HashMap<String, String>,
    timezone: &str,
) -> Answer {
    answer(log, || {
        let Some(dimension) = suggestion_dimension(filter) else {
            return Err(QueryError(format!("There are no suggestions for '{filter}'.")).into());
        };

        let period = params.get("period").map(String::as_str).unwrap_or("");
        let date_range = if period == "custom" {
            json!([params.get("from"), params.get("to")])
        } else {
            json!(Period::parse(period).unwrap_or(Period::Days(28)).name())
        };
        let filters: Value = match params.get("filters").filter(|f| !f.is_empty()) {
            Some(raw) => serde_json::from_str(raw).map_err(|e| QueryError(e.to_string()))?,
            None => json!([]),
        };

        // Validated as the query form is, since it is the same question.
        let query = parse_stats_query(&json!({
            "date_range": date_range,
            "relative_date": params.get("date").filter(|d| !d.is_empty()),
            "metrics": ["visitors"],
            "filters": filters,
        }))?;

        QueryEngine::new(conn, now).suggestions(host, dimension, query, params.get("q").map_or("", String::as_str), timezone)
    })
}

fn answer<T: Serialize>(log: &Logger, run: impl FnOnce() -> Result<T, StatsError>) -> Answer {
    let failed = |error: &dyn std::fmt::Display| {
        log.error("analytics_query_failed", fields! { "err_msg" => error.to_string() });
        Answer::error(StatusCode::INTERNAL_SERVER_ERROR, "The analytics query failed. The server log has the details.")
    };
    match run() {
        Ok(body) => match serde_json::to_string(&body) {
            Ok(body) => Answer { status: StatusCode::OK, body },
            Err(error) => failed(&error),
        },
        Err(StatsError::Query(error)) => Answer::error(StatusCode::BAD_REQUEST, &error.0),
        Err(StatsError::Internal(error)) => failed(&error),
    }
}
