//! The dashboard's questions — a query, the live visitor count, filter
//! suggestions, and the list of sites' sparklines — answered as the HTTP
//! responses Plausible's endpoints give.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{json, Value};

use super::query::engine::{QueryEngine, QueryHost, StatsError};
use super::query::parse::{parse_stats_query, QueryError};
use super::query::period::PERIODS;
use super::WorkerResponse;
use crate::fields;
use crate::log::Logger;

/// The suggestions endpoint's filter names, and the dimension each one means.
fn suggestion_dimension(filter: &str) -> Option<&'static str> {
    Some(match filter {
        "page" => "event:page",
        "entry_page" => "visit:entry_page",
        "exit_page" => "visit:exit_page",
        "source" => "visit:source",
        "channel" => "visit:channel",
        "referrer" => "visit:referrer",
        "utm_medium" => "visit:utm_medium",
        "utm_source" => "visit:utm_source",
        "utm_campaign" => "visit:utm_campaign",
        "utm_content" => "visit:utm_content",
        "utm_term" => "visit:utm_term",
        "country" => "visit:country",
        "region" => "visit:region",
        "city" => "visit:city",
        "screen" => "visit:device",
        "browser" => "visit:browser",
        "browser_version" => "visit:browser_version",
        "os" => "visit:os",
        "os_version" => "visit:os_version",
        _ => return None,
    })
}

pub fn query(conn: &Connection, log: &Logger, now: i64, host: &QueryHost, body: &Value, timezone: &str) -> WorkerResponse {
    answer(log, || {
        let query = parse_stats_query(body)?;
        QueryEngine::new(conn, now).query(host, &query, timezone)
    })
}

pub fn current_visitors(conn: &Connection, log: &Logger, now: i64, host: &QueryHost) -> WorkerResponse {
    answer(log, || Ok(json!(QueryEngine::new(conn, now).current_visitors(host)?)))
}

/// The last 24 hours of each site: unique visitors, their change against the
/// 24 hours before, and the visitors of each hour. Two queries a site, since
/// hourly buckets don't add up to a day's unique visitors.
pub fn sparklines(conn: &Connection, log: &Logger, now: i64, hosts: &[QueryHost], timezone: &str) -> WorkerResponse {
    answer(log, || {
        let total = parse_stats_query(&json!({
            "date_range": "24h",
            "metrics": ["visitors"],
            "include": { "compare": "previous_period", "compare_match_day_of_week": false },
        }))?;
        let series = parse_stats_query(&json!({
            "date_range": "24h",
            "metrics": ["visitors"],
            "dimensions": ["time:hour"],
            "include": { "time_labels": true },
        }))?;

        let engine = QueryEngine::new(conn, now);
        let mut sparklines = serde_json::Map::new();
        for host in hosts {
            let total = engine.query(host, &total, timezone)?;
            let hourly = engine.query(host, &series, timezone)?;

            let row = &total["results"][0];
            let hours: Vec<Value> = hourly["meta"]["time_label_result_indices"]
                .as_array()
                .map(|indices| {
                    indices
                        .iter()
                        .map(|index| match index.as_u64() {
                            Some(i) => hourly["results"][i as usize]["metrics"][0].clone(),
                            None => json!(0),
                        })
                        .collect()
                })
                .unwrap_or_default();

            sparklines.insert(
                host.hostname.clone(),
                json!({
                    "visitors": if row["metrics"][0].is_null() { json!(0) } else { row["metrics"][0].clone() },
                    "change": row["comparison"]["change"][0].clone(),
                    "hours": hours,
                }),
            );
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
) -> WorkerResponse {
    answer(log, || {
        let Some(dimension) = suggestion_dimension(filter) else {
            return Err(QueryError(format!("There are no suggestions for '{filter}'.")).into());
        };

        let period = params.get("period").map(String::as_str).unwrap_or("");
        let date_range = if period == "custom" {
            json!([params.get("from"), params.get("to")])
        } else if PERIODS.contains(&period) {
            json!(period)
        } else {
            json!("28d")
        };
        let filters: Value = match params.get("filters").filter(|f| !f.is_empty()) {
            Some(raw) => serde_json::from_str(raw).map_err(|e| QueryError(e.to_string()))?,
            None => json!([]),
        };

        let query = parse_stats_query(&json!({
            "date_range": date_range,
            "relative_date": params.get("date").filter(|d| !d.is_empty()),
            "metrics": ["visitors"],
            "filters": filters,
        }))?;

        QueryEngine::new(conn, now).suggestions(host, dimension, query, params.get("q").map_or("", String::as_str), timezone)
    })
}

fn answer(log: &Logger, run: impl FnOnce() -> Result<Value, StatsError>) -> WorkerResponse {
    match run() {
        Ok(body) => WorkerResponse::new(200, "application/json", body.to_string()),
        Err(StatsError::Query(error)) => WorkerResponse::new(400, "application/json", json!({ "error": error.0 }).to_string()),
        Err(StatsError::Internal(error)) => {
            log.error("analytics_query_failed", fields! { "err_msg" => error.to_string() });
            WorkerResponse::new(
                500,
                "application/json",
                json!({ "error": "The analytics query failed. The server log has the details." }).to_string(),
            )
        }
    }
}
