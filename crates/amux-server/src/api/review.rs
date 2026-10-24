//! GET /api/review/week?days=N — weekly trends data engine.
//! GET /api/review/digest?file= — weekly digest markdown.

use super::AppState;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/week", axum::routing::get(week))
        .route("/digest", axum::routing::get(digest))
}

#[derive(Deserialize)]
struct WeekQuery {
    #[serde(default = "default_days")]
    days: u32,
}

fn default_days() -> u32 {
    7
}

async fn week(State(state): State<AppState>, Query(q): Query<WeekQuery>) -> Response {
    let days = q.days.clamp(1, 90);
    let store = state.store.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let conn = store.read()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let table = crate::runtime_jobs::token_ledger::prices(&crate::config::amux_home());
        summarize_week(&conn, days, now, &table)
    })
    .await;

    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Weekly trends price counters with the same configured table as the Cost tab.
fn summarize_week(
    conn: &rusqlite::Connection,
    days: u32,
    now: i64,
    table: &[(String, [f64; 4])],
) -> anyhow::Result<serde_json::Value> {
    let since = now - (days as i64) * 86400;
    let since_ms = since * 1000;

    // Per-session aggregation
    let mut per: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();

    fn new_slot(sess: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("session".into(), json!(sess));
        m.insert("messages".into(), json!(0));
        m.insert("cards_created".into(), json!(0));
        m.insert("cards_done".into(), json!(0));
        m.insert("cards_verified".into(), json!(0));
        m.insert("tokens".into(), json!(0));
        m.insert("cost_usd".into(), json!(0.0));
        m.insert("samples".into(), json!([]));
        m
    }

    macro_rules! slot {
        ($per:expr, $sess:expr) => {{
            let key = $sess.to_string();
            if !$per.contains_key(&key) {
                $per.insert(key.clone(), new_slot(&key));
            }
            $per.get_mut(&key).unwrap()
        }};
    }

    // Messages from cmd_history
    {
        let mut stmt = conn.prepare(
            "SELECT session, text FROM cmd_history WHERE ts >= ?1
             AND (type IS NULL OR type NOT IN ('worker','schedule','system'))
             ORDER BY ts",
        )?;
        let rows = stmt.query_map([since_ms], |r| {
            Ok((
                r.get::<_, String>(0).unwrap_or_default(),
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        })?;
        for row in rows.flatten() {
            let (sess, text) = row;
            let s = slot!(per, sess);
            let msgs = s["messages"].as_i64().unwrap_or(0);
            s.insert("messages".into(), json!(msgs + 1));
            if let Some(arr) = s
                .get_mut("samples")
                .and_then(|v: &mut serde_json::Value| v.as_array_mut())
            {
                if arr.len() < 5 && !text.is_empty() {
                    let clean = text
                        .trim_start_matches('[')
                        .split_once(']')
                        .map(|(_, rest)| rest.trim())
                        .unwrap_or(&text);
                    let truncated: String = clean.chars().take(140).collect();
                    arr.push(json!(truncated));
                }
            }
        }
    }

    // Cards from issues
    {
        let mut stmt = conn.prepare(
            "SELECT session, status FROM issues WHERE created >= ?1 AND deleted IS NULL",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0).unwrap_or_default(),
                r.get::<_, String>(1).unwrap_or_default(),
            ))
        })?;
        for row in rows.flatten() {
            let (sess, status) = row;
            let s = slot!(per, sess);
            let cc = s["cards_created"].as_i64().unwrap_or(0);
            s.insert("cards_created".into(), json!(cc + 1));
            match status.as_str() {
                "done" => {
                    let cd = s["cards_done"].as_i64().unwrap_or(0);
                    s.insert("cards_done".into(), json!(cd + 1));
                }
                "verified" => {
                    let cv = s["cards_verified"].as_i64().unwrap_or(0);
                    s.insert("cards_verified".into(), json!(cv + 1));
                }
                _ => {}
            }
        }
    }

    // Group counters by model before pricing; stored money is ingest-time evidence.
    let mut unpriced_turns = 0;
    let mut n_considered = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT session, model, SUM(COALESCE(input,0)), SUM(COALESCE(cache_read,0)),
                    SUM(COALESCE(cache_write,0)), SUM(COALESCE(output,0)), COUNT(*)
             FROM token_ledger WHERE ts >= ?1 GROUP BY session, model",
        )?;
        let rows = stmt.query_map([since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                [r.get::<_, i64>(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
                r.get::<_, i64>(6)?,
            ))
        })?;
        for row in rows {
            let (sess, model, tokens, turns) = row?;
            let cost = crate::runtime_jobs::token_ledger::turn_cost_usd(table, &model, tokens);
            let s = slot!(per, sess);
            let previous_tokens = s["tokens"].as_i64().unwrap_or(0);
            let previous_cost = s["cost_usd"].as_f64().unwrap_or(0.0);
            s.insert(
                "tokens".into(),
                json!(previous_tokens + tokens[0] + tokens[3]),
            );
            s.insert("cost_usd".into(), json!(previous_cost + cost));
            n_considered += turns;
            if !crate::runtime_jobs::token_ledger::model_is_priced(table, &model) {
                unpriced_turns += turns;
            }
        }
    }

    let mut rows: Vec<serde_json::Value> =
        per.into_values().map(serde_json::Value::Object).collect();
    rows.sort_by(|a, b| {
        let score = |v: &serde_json::Value| {
            v["messages"].as_i64().unwrap_or(0) + v["cards_created"].as_i64().unwrap_or(0)
        };
        score(b).cmp(&score(a))
    });

    let totals = json!({
        "messages": rows.iter().map(|r| r["messages"].as_i64().unwrap_or(0)).sum::<i64>(),
        "cards_created": rows.iter().map(|r| r["cards_created"].as_i64().unwrap_or(0)).sum::<i64>(),
        "cards_done": rows.iter().map(|r| r["cards_done"].as_i64().unwrap_or(0)).sum::<i64>(),
        "cards_verified": rows.iter().map(|r| r["cards_verified"].as_i64().unwrap_or(0)).sum::<i64>(),
        "tokens": rows.iter().map(|r| r["tokens"].as_i64().unwrap_or(0)).sum::<i64>(),
        "cost_usd": (rows.iter().map(|r| r["cost_usd"].as_f64().unwrap_or(0.0)).sum::<f64>() * 100.0).round() / 100.0,
        "active_sessions": rows.iter().filter(|r| r["messages"].as_i64().unwrap_or(0) > 0 || r["cards_created"].as_i64().unwrap_or(0) > 0).count(),
    });

    for row in &mut rows {
        let cost = row["cost_usd"].as_f64().unwrap_or(0.0);
        row["cost_usd"] = json!((cost * 100.0).round() / 100.0);
    }
    tracing::info!(
        verdict = "usage_list_price_repriced",
        surface = "review_week",
        measured = true,
        n_considered,
        unpriced_turns,
        cost_basis = super::observability::COST_BASIS,
        "priced token counters with the configured list-price table"
    );
    Ok(json!({
        "cost_basis": super::observability::price_basis(table),
        "unpriced_turns": unpriced_turns,
        "billing_measured": false,
        "billed_usd": serde_json::Value::Null,
        "days": days,
        "since": since,
        "totals": totals,
        "per_session": rows,
    }))
}

#[derive(Deserialize)]
struct DigestQuery {
    #[serde(default)]
    file: String,
}

fn digest_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Dev/amux/docs/weekly-review")
}

async fn digest(Query(q): Query<DigestQuery>) -> Response {
    let dir = digest_dir();

    if !q.file.is_empty() {
        // Serve a specific digest file
        let safe_name = q.file.replace(['/', '\\', '\0'], "").trim().to_string();
        let path = dir.join(&safe_name);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                return Json(json!({"file": safe_name, "markdown": content})).into_response()
            }
            Err(_) => {
                return (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(json!({"error": "digest not found"})),
                )
                    .into_response()
            }
        }
    }

    // List available digests
    let mut weeks: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") {
                weeks.push(json!({"file": name}));
            }
        }
    }
    weeks.sort_by(|a, b| {
        let af = a["file"].as_str().unwrap_or("");
        let bf = b["file"].as_str().unwrap_or("");
        bf.cmp(af)
    });

    // Return the latest digest content too
    let latest = weeks.first().and_then(|w| {
        let f = w["file"].as_str()?;
        let content = std::fs::read_to_string(dir.join(f)).ok()?;
        Some(json!({"file": f, "markdown": content}))
    });

    Json(json!({
        "weeks": weeks,
        "file": latest.as_ref().and_then(|l| l["file"].as_str()),
        "markdown": latest.as_ref().and_then(|l| l["markdown"].as_str()),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_review_reprices_each_model_before_session_and_fleet_totals() {
        let conn = crate::db::migrate::test_memdb();
        let now = 1_789_400_000;
        let table = vec![
            ("fixture-a".into(), [1.0, 2.0, 3.0, 4.0]),
            ("fixture-b".into(), [2.0, 4.0, 6.0, 8.0]),
            ("fixture-free".into(), [0.0; 4]),
        ];
        for (session, model, at) in [
            ("mixed", "fixture-a", now - 60),
            ("mixed", "fixture-b", now - 50),
            ("unpriced", "unknown-vendor", now - 40),
            ("free", "fixture-free", now - 30),
            ("excluded", "fixture-a", now - 86_401),
        ] {
            conn.execute(
                "INSERT INTO token_ledger(ts,session,conversation,model,input,cache_read,cache_write,output,cost_usd,task) VALUES(?1,?2,'c',?3,1000000,1000000,1000000,1000000,999.0,'')",
                rusqlite::params![at, session, model],
            ).unwrap();
        }
        let value = summarize_week(&conn, 1, now, &table).unwrap();
        assert_eq!(value["totals"]["cost_usd"], json!(30.0));
        assert_eq!(value["totals"]["tokens"], json!(8_000_000));
        let rows = value["per_session"].as_array().unwrap();
        let mixed = rows.iter().find(|r| r["session"] == "mixed").unwrap();
        assert_eq!(mixed["cost_usd"], json!(30.0));
        assert_eq!(mixed["tokens"], json!(4_000_000));
        for session in ["unpriced", "free"] {
            assert_eq!(
                rows.iter().find(|r| r["session"] == session).unwrap()["cost_usd"],
                json!(0.0)
            );
        }
        assert!(!rows.iter().any(|r| r["session"] == "excluded"));
        assert_eq!(value["unpriced_turns"], json!(1));
        assert_eq!(
            value["cost_basis"],
            super::super::observability::price_basis(&table)
        );
        assert_eq!(value["billing_measured"], false);
        assert!(value["billed_usd"].is_null());
        let empty = summarize_week(&conn, 1, now + 86_401, &table).unwrap();
        assert_eq!(empty["totals"]["cost_usd"], json!(0.0));
        assert_eq!(empty["per_session"], json!([]));
        assert_eq!(empty["cost_basis"], value["cost_basis"]);
        let missing_schema = rusqlite::Connection::open_in_memory().unwrap();
        assert!(summarize_week(&missing_schema, 1, now, &table).is_err());
    }

    #[test]
    fn weekly_review_rounds_after_aggregating_models_and_sessions() {
        let conn = crate::db::migrate::test_memdb();
        let table = vec![("fixture".into(), [1.0, 0.0, 0.0, 0.0])];
        for session in ["one", "two"] {
            conn.execute(
                "INSERT INTO token_ledger(ts,session,conversation,model,input,cache_read,cache_write,output,cost_usd,task) VALUES(100,?1,'c','fixture',6000,0,0,0,999.0,'')",
                [session],
            ).unwrap();
        }
        let value = summarize_week(&conn, 1, 101, &table).unwrap();
        assert_eq!(value["totals"]["cost_usd"], json!(0.01));
    }
}
