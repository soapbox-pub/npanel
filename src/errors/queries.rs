//! What the dashboard asks of the errors database. Blocking; run on the
//! errors runtime's read connections.

use std::collections::HashMap;

use anyhow::Result;
use http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

use super::store::decompress;
use crate::analytics::query::engine::QueryHost;
use crate::analytics::Answer;

/// Issues listed at once.
const PAGE: i64 = 50;
/// Events listed at once.
const EVENTS_PAGE: i64 = 50;
const HOUR: i64 = 3600;
/// How far back an issue's own page charts it.
const ISSUE_HOURS: i64 = 14 * 24;

pub const STATUSES: [&str; 3] = ["unresolved", "resolved", "ignored"];

const ISSUE_COLUMNS: &str = "id, status, title, culprit, level, first_seen, last_seen, events, visitors, first_release, last_release";

fn issue_json(row: &Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, i64>(0)?,
        "status": row.get::<_, String>(1)?,
        "title": row.get::<_, String>(2)?,
        "culprit": row.get::<_, String>(3)?,
        "level": row.get::<_, String>(4)?,
        "first_seen": row.get::<_, i64>(5)?,
        "last_seen": row.get::<_, i64>(6)?,
        "events": row.get::<_, i64>(7)?,
        "visitors": row.get::<_, i64>(8)?,
        "first_release": row.get::<_, Option<String>>(9)?,
        "last_release": row.get::<_, Option<String>>(10)?,
    }))
}

fn ok(body: Value) -> Answer {
    Answer { status: StatusCode::OK, body: body.to_string() }
}

fn no_issue() -> Answer {
    Answer::error(StatusCode::NOT_FOUND, "This site has no such issue. It may have been removed after going quiet for longer than errors are kept.")
}

/// The hour `now` falls in.
fn hour_of(now: i64) -> i64 {
    now - now.rem_euclid(HOUR)
}

/// A page of one site's issues in one state, the most recently seen first,
/// each with its last 24 hours hour by hour; how many it has in each state;
/// and where the next page starts, if there is one.
pub fn issues(conn: &Connection, host_id: i64, status: &str, cursor: Option<&str>, now: i64) -> Result<Answer> {
    if !STATUSES.contains(&status) {
        return Ok(Answer::error(StatusCode::BAD_REQUEST, "Status must be unresolved, resolved, or ignored."));
    }
    let after = match cursor {
        None => None,
        Some(cursor) => match cursor.split_once('.').and_then(|(seen, id)| Some((seen.parse::<i64>().ok()?, id.parse::<i64>().ok()?))) {
            Some(after) => Some(after),
            None => return Ok(Answer::error(StatusCode::BAD_REQUEST, "That cursor isn't one this API gave out.")),
        },
    };
    let (seen, id) = after.unwrap_or((i64::MAX, i64::MAX));

    let mut rows: Vec<Value> = conn
        .prepare_cached(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issues WHERE host_id = ? AND status = ? AND (last_seen, id) < (?, ?)
             ORDER BY last_seen DESC, id DESC LIMIT ?"
        ))?
        .query_map(params![host_id, status, seen, id, PAGE + 1], issue_json)?
        .collect::<rusqlite::Result<_>>()?;
    let more = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);

    // Each issue's last 24 hours, oldest first, ending with this one.
    let start = hour_of(now) - 23 * HOUR;
    let ids: Vec<i64> = rows.iter().filter_map(|r| r["id"].as_i64()).collect();
    let mut hours: HashMap<i64, Vec<i64>> = ids.iter().map(|id| (*id, vec![0; 24])).collect();
    if !ids.is_empty() {
        let placeholders = vec!["?"; ids.len()].join(", ");
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&start];
        values.extend(ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        let mut statement = conn.prepare(&format!("SELECT issue_id, hour, events FROM issue_hours WHERE hour >= ? AND issue_id IN ({placeholders})"))?;
        let counts = statement.query_map(values.as_slice(), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?;
        for count in counts {
            let (issue, hour, events) = count?;
            if let Some(slot) = hours.get_mut(&issue).and_then(|h| h.get_mut(((hour - start) / HOUR) as usize)) {
                *slot = events;
            }
        }
    }
    for row in &mut rows {
        let id = row["id"].as_i64().unwrap_or_default();
        row["hours"] = json!(hours.remove(&id).unwrap_or_default());
    }

    let next = more.then(|| rows.last().map(|r| format!("{}.{}", r["last_seen"], r["id"]))).flatten();

    let mut counts: HashMap<String, i64> = STATUSES.iter().map(|s| (s.to_string(), 0)).collect();
    let mut statement = conn.prepare_cached("SELECT status, COUNT(*) FROM issues WHERE host_id = ? GROUP BY status")?;
    for row in statement.query_map([host_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (status, count) = row?;
        counts.insert(status, count);
    }

    Ok(ok(json!({ "issues": rows, "counts": counts, "next": next })))
}

/// One issue, with its last two weeks hour by hour, oldest first, and its
/// newest event's id.
pub fn issue(conn: &Connection, host_id: i64, issue_id: i64, now: i64) -> Result<Answer> {
    let Some(mut issue) = conn
        .prepare_cached(&format!("SELECT {ISSUE_COLUMNS} FROM issues WHERE id = ? AND host_id = ?"))?
        .query_row(params![issue_id, host_id], issue_json)
        .optional()?
    else {
        return Ok(no_issue());
    };

    let since = hour_of(now) - (ISSUE_HOURS - 1) * HOUR;
    let mut hours = vec![0i64; ISSUE_HOURS as usize];
    let mut statement = conn.prepare_cached("SELECT hour, events FROM issue_hours WHERE issue_id = ? AND hour >= ?")?;
    for row in statement.query_map(params![issue_id, since], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (hour, events) = row?;
        if let Some(slot) = hours.get_mut(((hour - since) / HOUR) as usize) {
            *slot = events;
        }
    }
    let latest: Option<i64> = conn
        .prepare_cached("SELECT id FROM events WHERE issue_id = ? ORDER BY id DESC LIMIT 1")?
        .query_row([issue_id], |r| r.get(0))
        .optional()?;

    issue["hours"] = json!(hours);
    issue["latest_event"] = json!(latest);
    Ok(ok(json!({ "issue": issue })))
}

fn event_summary(row: &Row) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, i64>(0)?,
        "event_id": row.get::<_, String>(1)?,
        "received_at": row.get::<_, i64>(2)?,
        "release": row.get::<_, Option<String>>(3)?,
        "url": row.get::<_, Option<String>>(4)?,
        "browser": row.get::<_, String>(5)?,
        "os": row.get::<_, String>(6)?,
    }))
}

fn owns(conn: &Connection, host_id: i64, issue_id: i64) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM issues WHERE id = ? AND host_id = ?")?
        .query_row(params![issue_id, host_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// A page of an issue's kept events, newest first, starting below `before`.
pub fn events(conn: &Connection, host_id: i64, issue_id: i64, before: Option<i64>) -> Result<Answer> {
    if !owns(conn, host_id, issue_id)? {
        return Ok(no_issue());
    }
    let mut rows: Vec<Value> = conn
        .prepare_cached(
            "SELECT id, event_id, received_at, release, url, browser, os FROM events
             WHERE issue_id = ? AND id < ? ORDER BY id DESC LIMIT ?",
        )?
        .query_map(params![issue_id, before.unwrap_or(i64::MAX), EVENTS_PAGE + 1], event_summary)?
        .collect::<rusqlite::Result<_>>()?;
    let more = rows.len() as i64 > EVENTS_PAGE;
    rows.truncate(EVENTS_PAGE as usize);
    let next = more.then(|| rows.last().map(|r| r["id"].clone())).flatten();
    Ok(ok(json!({ "events": rows, "next": next })))
}

/// One event, whole, with the ids of the events either side of it.
pub fn event(conn: &Connection, host_id: i64, issue_id: i64, id: i64) -> Result<Answer> {
    if !owns(conn, host_id, issue_id)? {
        return Ok(no_issue());
    }
    let row = conn
        .prepare_cached("SELECT id, event_id, received_at, release, url, browser, os, payload FROM events WHERE id = ? AND issue_id = ?")?
        .query_row(params![id, issue_id], |r| Ok((event_summary(r)?, r.get::<_, Vec<u8>>(7)?)))
        .optional()?;
    let Some((mut event, payload)) = row else {
        return Ok(Answer::error(StatusCode::NOT_FOUND, "This issue no longer keeps that event. Only its first and latest events are kept."));
    };
    event["report"] = serde_json::from_slice(&decompress(&payload)?)?;

    let neighbor = |sql: &str| -> rusqlite::Result<Option<i64>> { conn.prepare_cached(sql)?.query_row(params![issue_id, id], |r| r.get(0)).optional() };
    let older = neighbor("SELECT id FROM events WHERE issue_id = ? AND id < ? ORDER BY id DESC LIMIT 1")?;
    let newer = neighbor("SELECT id FROM events WHERE issue_id = ? AND id > ? ORDER BY id LIMIT 1")?;
    Ok(ok(json!({ "event": event, "older": older, "newer": newer })))
}

/// For each site, how many issues are unresolved and how many reports came
/// in over the last 24 hours, by hostname.
pub fn summary(conn: &Connection, hosts: &[QueryHost], now: i64) -> Result<Answer> {
    let mut unresolved: HashMap<i64, i64> = HashMap::new();
    let mut statement = conn.prepare_cached("SELECT host_id, COUNT(*) FROM issues WHERE status = 'unresolved' GROUP BY host_id")?;
    for row in statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (host, count) = row?;
        unresolved.insert(host, count);
    }
    let mut recent: HashMap<i64, i64> = HashMap::new();
    let mut statement = conn.prepare_cached(
        "SELECT issues.host_id, SUM(issue_hours.events) FROM issue_hours JOIN issues ON issues.id = issue_hours.issue_id
         WHERE issue_hours.hour >= ? GROUP BY issues.host_id",
    )?;
    for row in statement.query_map([hour_of(now) - 23 * HOUR], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (host, count) = row?;
        recent.insert(host, count);
    }

    let body: serde_json::Map<String, Value> = hosts
        .iter()
        .map(|h| {
            (h.hostname.clone(), json!({ "unresolved": unresolved.get(&h.id).copied().unwrap_or(0), "events_24h": recent.get(&h.id).copied().unwrap_or(0) }))
        })
        .collect();
    Ok(ok(Value::Object(body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::grouping::Grouping;
    use crate::errors::store::{compress, ErrorsStore, Occurrence};

    fn occurrence(host_id: i64, fingerprint: &str, at: i64) -> Occurrence {
        Occurrence {
            host_id,
            grouping: Grouping { fingerprint: fingerprint.into(), title: format!("Error: {fingerprint}"), culprit: "f (a.js)".into() },
            level: "error".into(),
            received_at: at,
            release: None,
            visitor: at,
            event_id: format!("{at:032x}"),
            url: None,
            browser: "Firefox".into(),
            os: "Linux".into(),
            payload: compress(format!("{{\"at\":{at}}}").as_bytes()),
        }
    }

    fn body(answer: Answer) -> Value {
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        serde_json::from_str(&answer.body).unwrap()
    }

    #[test]
    fn answers_the_dashboard() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("errors.sqlite3");
        let mut store = ErrorsStore::new(crate::db::open_connection(&path).unwrap()).unwrap();
        let now = 100 * HOUR + 30;
        let mut batch: Vec<Occurrence> = (0..60).map(|i| occurrence(1, &format!("i{i}"), now - 3 * HOUR - i)).collect();
        batch.extend((0..3).map(|i| occurrence(1, "hot", now - i * HOUR)));
        batch.push(occurrence(2, "other", now));
        store.write(&batch).unwrap();
        store.set_status(1, 1, "resolved").unwrap();
        let conn = crate::db::open_connection(&path).unwrap();

        let page = body(issues(&conn, 1, "unresolved", None, now).unwrap());
        let list = page["issues"].as_array().unwrap();
        assert_eq!(list.len(), 50);
        assert_eq!(list[0]["title"], "Error: hot");
        assert_eq!(list[0]["events"], 3);
        let hours: Vec<i64> = serde_json::from_value(list[0]["hours"].clone()).unwrap();
        assert_eq!(&hours[21..], [1, 1, 1]);
        assert_eq!(page["counts"], json!({ "unresolved": 60, "resolved": 1, "ignored": 0 }));
        let next = page["next"].as_str().unwrap();
        let rest = body(issues(&conn, 1, "unresolved", Some(next), now).unwrap());
        assert_eq!(rest["issues"].as_array().unwrap().len(), 10);
        assert_eq!(rest["next"], Value::Null);
        assert_eq!(issues(&conn, 1, "open", None, now).unwrap().status, StatusCode::BAD_REQUEST);
        assert_eq!(issues(&conn, 1, "unresolved", Some("x"), now).unwrap().status, StatusCode::BAD_REQUEST);

        let hot = list[0]["id"].as_i64().unwrap();
        let one = body(issue(&conn, 1, hot, now).unwrap());
        let hours: Vec<i64> = serde_json::from_value(one["issue"]["hours"].clone()).unwrap();
        assert_eq!(hours.len(), 336);
        assert_eq!(&hours[333..], [1, 1, 1]);
        // Another site's issue is no issue at all.
        assert_eq!(issue(&conn, 2, hot, now).unwrap().status, StatusCode::NOT_FOUND);

        let list = body(events(&conn, 1, hot, None).unwrap());
        let ids: Vec<i64> = list["events"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(one["issue"]["latest_event"], ids[0]);
        let middle = body(event(&conn, 1, hot, ids[1]).unwrap());
        assert_eq!((middle["older"].as_i64(), middle["newer"].as_i64()), (Some(ids[2]), Some(ids[0])));
        assert_eq!(middle["event"]["report"]["at"], now - HOUR);
        assert_eq!(event(&conn, 2, hot, ids[1]).unwrap().status, StatusCode::NOT_FOUND);
        assert_eq!(events(&conn, 2, hot, None).unwrap().status, StatusCode::NOT_FOUND);

        let hosts = [QueryHost { id: 1, hostname: "a.example.com".into() }, QueryHost { id: 2, hostname: "b.example.com".into() }, QueryHost { id: 3, hostname: "c.example.com".into() }];
        let all = body(summary(&conn, &hosts, now).unwrap());
        assert_eq!(all["a.example.com"], json!({ "unresolved": 60, "events_24h": 63 }));
        assert_eq!(all["b.example.com"], json!({ "unresolved": 1, "events_24h": 1 }));
        assert_eq!(all["c.example.com"], json!({ "unresolved": 0, "events_24h": 0 }));
    }
}
