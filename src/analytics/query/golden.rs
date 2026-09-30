//! What the dashboard is told, pinned: a fixed set of visits, a broad set of
//! the questions the dashboard asks about them, and the exact responses.
//!
//! The engine's answers are Plausible's semantics worked out by hand, and a
//! refactor that changes one number is a bug nobody would see. So every
//! answer is compared with the one recorded in `testdata/golden.json`. After
//! a change that is *meant* to alter an answer, rerun with `UPDATE_GOLDEN=1`
//! and read the diff.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::analytics::query::engine::QueryHost;
use crate::analytics::stats;
use crate::analytics::store::AnalyticsStore;
use crate::log::{LogLevel, Logger};

/// 2026-09-30T12:34:56Z, a Wednesday.
const NOW: i64 = 1_790_771_696;

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/analytics/query/testdata/golden.json");

/// A small deterministic generator, so the visits are the same every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

/// Visits over the last fourteen months, busier lately, with a few still
/// going on now.
fn seed(conn: &Connection) {
    conn.execute_batch("BEGIN").unwrap();
    let pages = ["/", "/about", "/blog", "/blog/post-1", "/blog/post-2", "/Contact", "/docs/intro"];
    let sources = [("", "", "Direct"), ("Google", "https://www.google.com/", "Organic Search"), ("Nostr", "https://njump.me/", "Referral"), ("Bing", "https://bing.com/", "Organic Search")];
    let places = [("DE", "Berlin", "Berlin"), ("US", "California", "San Francisco"), ("US", "New York", "New York"), ("", "", ""), ("FR", "Île-de-France", "Paris")];
    let devices = [("Desktop", "Chrome", "128.0", "Windows", "10"), ("Mobile", "Safari", "17.4", "iOS", "17.4"), ("Desktop", "Firefox", "130.0", "Linux", ""), ("Tablet", "", "", "", "")];
    let utm = ["", "", "", "newsletter", "social"];

    let mut rng = Lcg(42);
    let mut session_id = 1_000;
    let mut event_id = 1;

    // A visit every few hours a year ago, every half hour or so lately, and a
    // few still going on now.
    let mut starts = Vec::new();
    let mut start = NOW - 430 * 86_400;
    loop {
        let age = (NOW - start) as u64;
        start += 600 + rng.below(if age > 60 * 86_400 { 30_000 } else { 3_000 }) as i64;
        if start >= NOW - 3_600 {
            break;
        }
        starts.push(start);
    }
    starts.extend([NOW - 1_500, NOW - 900, NOW - 240]);

    for start in starts {
        session_id += 1;
        let host_id = 1;
        let user_id = 1 + rng.below(250) as i64;
        let (source, referrer, channel) = sources[rng.below(sources.len() as u64) as usize];
        let (country, region, city) = places[rng.below(places.len() as u64) as usize];
        let (device, browser, browser_version, os, os_version) = devices[rng.below(devices.len() as u64) as usize];
        let medium = rng.pick(&utm);

        let views = 1 + rng.below(4) as i64;
        let mut at = start;
        let mut visited = Vec::new();
        for view in 0..views {
            if view > 0 {
                at += 5 + rng.below(240) as i64;
            }
            let page = rng.pick(&pages);
            visited.push(page);
            insert_event(conn, &mut event_id, host_id, at, 0, session_id, user_id, page, 0, 255);
            if rng.below(3) > 0 {
                let engaged = 1_000 + rng.below(90_000) as i64;
                insert_event(conn, &mut event_id, host_id, at + 3, 1, session_id, user_id, page, engaged, rng.below(101) as i64);
            }
        }
        if rng.below(10) == 0 {
            insert_event(conn, &mut event_id, host_id, at + 1, 2, session_id, user_id, visited[visited.len() - 1], 0, 255);
        }
        let is_bounce = views == 1 && rng.below(4) > 0;

        conn.execute(
            "INSERT INTO sessions (host_id, start, session_id, user_id, timestamp, entry_page, exit_page, pageviews, events, duration,
               is_bounce, referrer, source, channel, utm_medium, utm_source, utm_campaign, utm_content, utm_term, click_id_param,
               country, region, city, device, browser, browser_version, os, os_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '', '', '', ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                host_id,
                start,
                session_id,
                user_id,
                at,
                visited[0],
                visited[visited.len() - 1],
                views,
                views,
                at - start,
                is_bounce,
                referrer,
                source,
                channel,
                medium,
                if medium.is_empty() { "" } else { "mail" },
                if medium.is_empty() { "" } else { "autumn" },
                country,
                region,
                city,
                device,
                browser,
                browser_version,
                os,
                os_version,
            ],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_event(conn: &Connection, id: &mut i64, host_id: i64, timestamp: i64, kind: i64, session_id: i64, user_id: i64, pathname: &str, engagement: i64, scroll: i64) {
    *id += 1;
    conn.execute(
        "INSERT INTO events (host_id, timestamp, id, kind, session_id, user_id, pathname, engagement_time, scroll_depth) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![host_id, timestamp, *id, kind, session_id, user_id, pathname, engagement, scroll],
    )
    .unwrap();
}

/// Every question asked, by name.
fn questions() -> Vec<(String, Value, &'static str)> {
    let mut asked: Vec<(String, Value, &'static str)> = Vec::new();
    let mut ask = |name: String, body: Value, zone: &'static str| asked.push((name, body, zone));

    let overview = json!(["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"]);
    for period in ["realtime", "realtime_30m", "day", "24h", "7d", "28d", "30d", "91d", "month", "6mo", "12mo", "year", "all"] {
        ask(format!("overview {period}"), json!({ "date_range": period, "metrics": overview }), "UTC");
    }
    ask("overview custom".into(), json!({ "date_range": ["2026-08-01", "2026-08-31"], "metrics": overview }), "UTC");
    ask("overview relative".into(), json!({ "date_range": "month", "relative_date": "2026-06-15", "metrics": overview }), "UTC");
    ask("overview day relative".into(), json!({ "date_range": "day", "relative_date": "2026-09-01", "metrics": overview }), "UTC");
    for zone in ["Europe/Berlin", "America/New_York", "Asia/Kolkata"] {
        ask(format!("overview 7d {zone}"), json!({ "date_range": "7d", "metrics": overview }), zone);
        ask(format!("days 7d {zone}"), json!({ "date_range": "7d", "metrics": ["visitors"], "dimensions": ["time:day"], "include": { "time_labels": true } }), zone);
        ask(format!("hours day {zone}"), json!({ "date_range": "day", "metrics": ["visitors", "visits"], "dimensions": ["time:hour"], "include": { "time_labels": true, "partial_time_labels": true, "present_index": true } }), zone);
    }

    for dimension in [
        "visit:source", "visit:referrer", "visit:channel", "visit:utm_medium", "visit:utm_source", "visit:utm_campaign", "visit:utm_content",
        "visit:utm_term", "visit:country", "visit:country_name", "visit:region", "visit:region_name", "visit:city", "visit:city_name",
        "visit:device", "visit:browser", "visit:browser_version", "visit:os", "visit:os_version", "visit:entry_page", "visit:exit_page",
    ] {
        ask(
            format!("breakdown {dimension}"),
            json!({ "date_range": "91d", "metrics": ["visitors", "visits", "bounce_rate", "visit_duration", "percentage"], "dimensions": [dimension], "pagination": { "limit": 5, "offset": 0 } }),
            "UTC",
        );
    }
    ask("countries named".into(), json!({ "date_range": "28d", "metrics": ["visitors"], "dimensions": ["visit:country", "visit:country_name"] }), "UTC");
    ask("pages".into(), json!({ "date_range": "28d", "metrics": ["visitors", "visits", "pageviews", "bounce_rate", "time_on_page", "scroll_depth", "percentage"], "dimensions": ["event:page"] }), "UTC");
    ask("pages scroll only".into(), json!({ "date_range": "28d", "metrics": ["scroll_depth"], "dimensions": ["event:page"] }), "UTC");
    ask("page filter".into(), json!({ "date_range": "28d", "metrics": ["visitors", "pageviews", "bounce_rate", "visit_duration", "time_on_page"], "filters": [["is", "event:page", ["/blog"]]] }), "UTC");
    ask("exit rate".into(), json!({ "date_range": "28d", "metrics": ["visitors", "visits", "exit_rate"], "dimensions": ["visit:exit_page"] }), "UTC");
    ask("pages by source".into(), json!({ "date_range": "91d", "metrics": ["visitors", "pageviews"], "dimensions": ["event:page", "visit:source"], "order_by": [["event:page", "asc"], ["visit:source", "desc"]] }), "UTC");

    for (interval, period) in [("day", "30d"), ("week", "91d"), ("week", "month"), ("month", "12mo"), ("month", "year"), ("hour", "day"), ("hour", "24h"), ("minute", "realtime_30m"), ("minute", "day")] {
        ask(
            format!("series {interval} {period}"),
            json!({ "date_range": period, "metrics": ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"], "dimensions": [format!("time:{interval}")], "include": { "time_labels": true, "partial_time_labels": true, "present_index": true } }),
            "UTC",
        );
    }
    ask("series by page".into(), json!({ "date_range": "7d", "metrics": ["visitors", "pageviews"], "dimensions": ["time:day", "event:page"], "include": { "time_labels": true } }), "UTC");

    for (compare, label) in [(json!("previous_period"), "previous"), (json!("year_over_year"), "yoy"), (json!(["2026-07-01", "2026-07-28"]), "custom")] {
        for match_dow in [false, true] {
            ask(
                format!("compare {label} dow={match_dow} total"),
                json!({ "date_range": "28d", "metrics": ["visitors", "bounce_rate", "visit_duration", "views_per_visit"], "include": { "compare": compare, "compare_match_day_of_week": match_dow } }),
                "UTC",
            );
        }
        ask(
            format!("compare {label} series"),
            json!({ "date_range": "28d", "metrics": ["visitors", "pageviews"], "dimensions": ["time:day"], "include": { "compare": compare, "time_labels": true, "partial_time_labels": true } }),
            "UTC",
        );
        ask(
            format!("compare {label} sources"),
            json!({ "date_range": "28d", "metrics": ["visitors", "bounce_rate"], "dimensions": ["visit:source"], "include": { "compare": compare } }),
            "UTC",
        );
    }
    for period in ["day", "24h", "month", "year", "6mo"] {
        for match_dow in [false, true] {
            ask(
                format!("compare previous {period} dow={match_dow}"),
                json!({ "date_range": period, "metrics": ["visitors", "visits"], "include": { "compare": "previous_period", "compare_match_day_of_week": match_dow } }),
                "Europe/Berlin",
            );
            ask(
                format!("compare yoy {period} dow={match_dow}"),
                json!({ "date_range": period, "metrics": ["visitors"], "include": { "compare": "year_over_year", "compare_match_day_of_week": match_dow } }),
                "America/New_York",
            );
        }
    }
    ask("compare exit rate".into(), json!({ "date_range": "28d", "metrics": ["visitors", "exit_rate"], "dimensions": ["visit:exit_page"], "include": { "compare": "previous_period" } }), "UTC");

    for (operator, dimension, clauses) in [
        ("is", "visit:source", json!(["Google"])),
        ("is_not", "visit:source", json!(["Google", "Direct / None"])),
        ("contains", "event:page", json!(["blog"])),
        ("contains_not", "event:page", json!(["blog"])),
        ("is", "visit:country", json!(["\u{0}\u{0}"])),
        ("is", "visit:city", json!([0])),
        ("is", "visit:entry_page", json!(["/about"])),
        ("contains", "visit:browser", json!(["fire"])),
    ] {
        ask(
            format!("filter {operator} {dimension}"),
            json!({ "date_range": "91d", "metrics": ["visitors", "visits", "pageviews", "bounce_rate"], "dimensions": ["visit:device"], "filters": [[operator, dimension, clauses]] }),
            "UTC",
        );
    }
    ask("filter case insensitive".into(), json!({ "date_range": "91d", "metrics": ["visitors"], "filters": [["contains", "event:page", ["contact"], { "case_sensitive": false }]] }), "UTC");
    ask("filter case sensitive".into(), json!({ "date_range": "91d", "metrics": ["visitors"], "filters": [["contains", "event:page", ["contact"]]] }), "UTC");
    ask("two filters".into(), json!({ "date_range": "91d", "metrics": ["visitors", "time_on_page"], "dimensions": ["event:page"], "filters": [["is", "visit:country", ["US"]], ["is_not", "event:page", ["/"]]] }), "UTC");

    ask("order asc".into(), json!({ "date_range": "91d", "metrics": ["visitors", "visits"], "dimensions": ["visit:browser"], "order_by": [["visits", "asc"]] }), "UTC");
    ask("order by dimension".into(), json!({ "date_range": "91d", "metrics": ["visitors"], "dimensions": ["visit:city_name"], "order_by": [["visit:city_name", "desc"]] }), "UTC");
    ask("paginated".into(), json!({ "date_range": "91d", "metrics": ["visitors"], "dimensions": ["event:page"], "pagination": { "limit": 2, "offset": 3 } }), "UTC");
    ask("empty metrics".into(), json!({ "date_range": "91d", "metrics": ["visitors", "visit_duration"], "dimensions": ["visit:source"], "include": { "empty_metrics": true, "imports": true } }), "UTC");
    ask("empty host".into(), json!({ "date_range": "28d", "metrics": overview, "dimensions": ["time:day"], "include": { "time_labels": true, "compare": "previous_period" } }), "UTC");

    for (name, body) in [
        ("unknown metric", json!({ "date_range": "28d", "metrics": ["goals"] })),
        ("no metrics", json!({ "date_range": "28d", "metrics": [] })),
        ("bad period", json!({ "date_range": "1w", "metrics": ["visitors"] })),
        ("backwards range", json!({ "date_range": ["2026-09-02", "2026-09-01"], "metrics": ["visitors"] })),
        ("bad dimension", json!({ "date_range": "28d", "metrics": ["visitors"], "dimensions": ["event:goal"] })),
        ("two time dimensions", json!({ "date_range": "28d", "metrics": ["visitors"], "dimensions": ["time:day", "time:week"] })),
        ("bad filter", json!({ "date_range": "28d", "metrics": ["visitors"], "filters": [["is", "visit:source"]] })),
        ("bad operator", json!({ "date_range": "28d", "metrics": ["visitors"], "filters": [["matches", "visit:source", ["x"]]] })),
        ("bad order", json!({ "date_range": "28d", "metrics": ["visitors"], "order_by": [["visits", "desc"]] })),
        ("bad pagination", json!({ "date_range": "28d", "metrics": ["visitors"], "pagination": { "limit": 0 } })),
        ("views per visit by page", json!({ "date_range": "28d", "metrics": ["views_per_visit"], "dimensions": ["event:page"] })),
        ("time on page alone", json!({ "date_range": "28d", "metrics": ["time_on_page"] })),
        ("exit rate alone", json!({ "date_range": "28d", "metrics": ["exit_rate"] })),
        ("compare all", json!({ "date_range": "all", "metrics": ["visitors"], "include": { "compare": "previous_period" } })),
        ("minutes of a week", json!({ "date_range": "7d", "metrics": ["visitors"], "dimensions": ["time:minute"] })),
        ("too many hours", json!({ "date_range": ["2020-01-01", "2026-01-01"], "metrics": ["visitors"], "dimensions": ["time:hour"] })),
        ("bad relative date", json!({ "date_range": "28d", "relative_date": "yesterday", "metrics": ["visitors"] })),
        ("bad compare", json!({ "date_range": "28d", "metrics": ["visitors"], "include": { "compare": "sometime" } })),
        ("not an object", json!(["visitors"])),
    ] {
        ask(format!("refused: {name}"), body, "UTC");
    }

    asked
}

/// Every answer, in the order asked.
fn answers() -> Vec<(String, Value)> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("analytics.sqlite3");
    AnalyticsStore::new(Connection::open(&path).unwrap()).unwrap();
    let conn = Connection::open(&path).unwrap();
    seed(&conn);

    let log = Logger::new(LogLevel::Error);
    let site = QueryHost { id: 1, hostname: "docs.example.com".into() };
    let empty = QueryHost { id: 2, hostname: "empty.example.com".into() };
    let recorded = |response: crate::analytics::WorkerResponse| {
        json!({ "status": response.status, "body": serde_json::from_str::<Value>(&response.body).unwrap_or(Value::String(response.body)) })
    };

    let mut answers = Vec::new();
    for (name, body, zone) in questions() {
        let host = if name == "empty host" { &empty } else { &site };
        answers.push((format!("{name} ({zone})"), recorded(stats::query(&conn, &log, NOW, host, &body, zone))));
    }

    answers.push(("current visitors".into(), recorded(stats::current_visitors(&conn, &log, NOW, &site))));
    for zone in ["UTC", "Europe/Berlin"] {
        answers.push((format!("sparklines ({zone})"), recorded(stats::sparklines(&conn, &log, NOW, &[site.clone(), empty.clone()], zone))));
    }

    let suggest = |filter: &str, extra: &[(&str, &str)]| -> Value {
        let params: HashMap<String, String> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        recorded(stats::suggestions(&conn, &log, NOW, &site, filter, &params, "UTC"))
    };
    for filter in [
        "page", "entry_page", "exit_page", "source", "channel", "referrer", "utm_medium", "utm_source", "utm_campaign", "utm_content",
        "utm_term", "country", "region", "city", "screen", "browser", "browser_version", "os", "os_version",
    ] {
        answers.push((format!("suggestions {filter}"), suggest(filter, &[("period", "91d")])));
    }
    answers.push(("suggestions typed".into(), suggest("page", &[("period", "91d"), ("q", "BLOG")])));
    answers.push(("suggestions custom".into(), suggest("source", &[("period", "custom"), ("from", "2026-08-01"), ("to", "2026-08-31")])));
    answers.push(("suggestions filtered".into(), suggest("page", &[("period", "30d"), ("filters", r#"[["is","visit:source",["Google"]]]"#)])));
    answers.push(("suggestions relative".into(), suggest("city", &[("period", "month"), ("date", "2026-07-10")])));
    answers.push(("suggestions default period".into(), suggest("os", &[])));
    answers.push(("suggestions unknown".into(), suggest("goal", &[])));
    answers.push(("suggestions bad filters".into(), suggest("page", &[("filters", "[")])));

    answers
}

#[test]
fn answers_are_unchanged() {
    let answers = answers();
    let recorded: Value = Value::Array(answers.iter().map(|(name, answer)| json!({ "question": name, "answer": answer })).collect());

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN).parent().unwrap()).unwrap();
        std::fs::write(GOLDEN, serde_json::to_string_pretty(&recorded).unwrap() + "\n").unwrap();
        return;
    }

    let golden: Value = serde_json::from_str(&std::fs::read_to_string(GOLDEN).expect("golden answers; run with UPDATE_GOLDEN=1 to record them")).unwrap();
    let golden = golden.as_array().unwrap();
    assert_eq!(golden.len(), answers.len(), "the set of questions changed; rerun with UPDATE_GOLDEN=1");
    for (expected, (name, answer)) in golden.iter().zip(&answers) {
        assert_eq!(expected["question"], json!(name));
        assert_eq!(&expected["answer"], answer, "{name}");
    }
}

#[test]
fn the_seed_has_something_to_say() {
    // Guards the golden file against recording a pile of zeros.
    let answers = answers();
    let overview = &answers.iter().find(|(name, _)| name == "overview 28d (UTC)").unwrap().1;
    assert!(overview["body"]["results"][0]["metrics"][0].as_i64().unwrap() > 50, "{overview}");
    let realtime = &answers.iter().find(|(name, _)| name == "overview realtime_30m (UTC)").unwrap().1;
    assert!(realtime["body"]["results"][0]["metrics"][0].as_i64().unwrap() > 0, "{realtime}");
}
