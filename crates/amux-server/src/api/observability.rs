//! Cost rollup and paginated response evidence share one pricing/provenance
//! contract with the dashboard and markdown report. Dollars are equivalents.
use super::AppState;
use crate::api::fs::{parse_qs, qs_get};
use crate::api::history::group_members;
use crate::runtime_jobs::{token_ledger, work_attribution::Attribution};
use axum::{
    extract::{RawQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) const COST_BASIS: &str = "list-price equivalent on Max subscriptions, not billed";
const SECONDS_PER_DAY: i64 = 86_400;
const MAX_PAGE_SIZE: i64 = 500;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/observability", get(rollup))
        .route("/api/observability/ledger", get(ledger))
}

#[derive(Clone, Default)]
pub(crate) struct Scope {
    pub since: i64,
    pub until: i64,
    pub session: String,
    pub worker_id: String,
    pub unknown_worker: bool,
    pub unattributed: bool,
    pub conversation: String,
    pub work_kind: String,
    pub work_id: String,
    pub members: Option<Vec<String>>,
}
impl Scope {
    pub(crate) fn sql(&self) -> (String, Vec<SqlValue>) {
        let mut sql = "ts >= ? AND ts < ?".to_string();
        let mut args = vec![SqlValue::Integer(self.since), SqlValue::Integer(self.until)];
        for (column, value) in [
            ("session", &self.session),
            ("worker_id", &self.worker_id),
            ("conversation", &self.conversation),
            (
                "json_extract(attribution,'$.primary.kind')",
                &self.work_kind,
            ),
            ("json_extract(attribution,'$.primary.id')", &self.work_id),
        ] {
            if !value.is_empty() {
                if column == "session" {
                    sql.push_str(" AND (session = ? OR worker_id = (SELECT MIN(id) FROM usage_workers w WHERE w.display_name=? OR w.id=? OR EXISTS(SELECT 1 FROM json_each(w.name_aliases) WHERE value=?) HAVING COUNT(*)=1))");
                    args.extend((0..4).map(|_| SqlValue::Text(value.clone())));
                    continue;
                }
                sql.push_str(&format!(" AND {column} = ?"));
                args.push(SqlValue::Text(value.clone()));
            }
        }
        if self.unattributed {
            sql.push_str(" AND json_extract(attribution,'$.primary') IS NULL");
        }
        if self.unknown_worker {
            sql.push_str(" AND COALESCE(worker_id,'') = '' AND session = ''");
        }
        if let Some(members) = &self.members {
            let holes = vec!["?"; members.len()].join(",");
            sql.push_str(&format!(" AND (session IN ({holes}) OR worker_id IN (SELECT id FROM usage_workers WHERE display_name IN ({holes})))"));
            args.extend(members.iter().cloned().map(SqlValue::Text));
            args.extend(members.iter().cloned().map(SqlValue::Text));
        }
        (sql, args)
    }
}

pub(crate) fn scope(query: &str, now: i64) -> Result<(Scope, i64, String), String> {
    let p = parse_qs(query);
    let integer = |key: &str, default: i64| -> Result<i64, String> {
        qs_get(&p, key)
            .map(|v| v.parse().map_err(|_| format!("invalid {key}")))
            .unwrap_or(Ok(default))
    };
    let days = integer("days", 7)?.clamp(1, 365);
    let until = integer("until", now + 1)?;
    let default_since = until
        .checked_sub(days * SECONDS_PER_DAY)
        .ok_or("invalid until boundary")?;
    let since = integer("since", default_since)?;
    if since >= until {
        return Err("since must be earlier than until (exclusive)".into());
    }
    let get = |key| qs_get(&p, key).unwrap_or_default().trim().to_string();
    let session = get("session");
    let worker_id = get("worker_id");
    let unknown_worker = match get("unknown_worker").as_str() {
        "" | "false" | "0" => false,
        "true" | "1" => true,
        _ => return Err("unknown_worker must be true or false".into()),
    };
    if unknown_worker && (!session.is_empty() || !worker_id.is_empty()) {
        return Err("unknown_worker cannot be combined with session or worker_id".into());
    }
    let unattributed = match get("unattributed").as_str() {
        "" | "0" | "false" => false,
        "1" | "true" => true,
        _ => return Err("unattributed must be true or false".into()),
    };
    let work_kind = get("work_kind");
    let work_id = get("work_id");
    if (unattributed && !work_id.is_empty())
        || work_kind.is_empty() != work_id.is_empty()
        || (!work_kind.is_empty() && !["card", "pr", "issue", "deal"].contains(&work_kind.as_str()))
    {
        return Err("work_kind and work_id must name card, pr, issue or deal".into());
    }
    let group = get("group");
    let members = if session.is_empty() && worker_id.is_empty() && !group.is_empty() {
        Some(group_members(&crate::config::amux_home(), &group))
    } else {
        None
    };
    Ok((
        Scope {
            since,
            until,
            session,
            worker_id,
            unknown_worker,
            unattributed,
            conversation: get("conversation"),
            work_kind,
            work_id,
            members,
        },
        days,
        group,
    ))
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub id: i64,
    pub ts: i64,
    pub session: String,
    pub conversation: String,
    pub model: String,
    pub tokens: [i64; 4],
    pub stored_cost: f64,
    pub response_id: Option<String>,
    pub request_id: Option<String>,
    pub attribution: Attribution,
    pub parent: Option<String>,
    pub compaction: Option<Value>,
}

fn entries(
    conn: &Connection,
    scope: &Scope,
    page: Option<(i64, i64)>,
) -> rusqlite::Result<Vec<Entry>> {
    let (mut filter, mut args) = scope.sql();
    if let Some((after, _)) = page {
        filter.push_str(" AND id > ?");
        args.push(SqlValue::Integer(after));
    }
    let mut sql=format!("SELECT id,ts,session,conversation,model,input,cache_read,cache_write,output,cost_usd,message_id,request_id,attribution,worker_id,task,parent_conversation,compaction FROM token_ledger WHERE {filter} ORDER BY id");
    if let Some((_, limit)) = page {
        sql.push_str(" LIMIT ?");
        args.push(SqlValue::Integer(limit + 1));
    }
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params_from_iter(args), |r| {
        let session: String = r.get(2)?;
        let conversation: String = r.get(3)?;
        let ts = r.get(1)?;
        let parent: Option<String> = r.get(15)?;
        let attribution = r
            .get::<_, Option<String>>(12)?
            .and_then(|a| serde_json::from_str(&a).ok())
            .unwrap_or(Attribution {
                worker_id: r.get(13)?,
                reason: "historical_attribution_pending".into(),
                source: "legacy_ledger".into(),
                effective_at: ts,
                ..Default::default()
            });
        Ok(Entry {
            id: r.get(0)?,
            ts,
            session,
            conversation,
            model: r.get(4)?,
            tokens: [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
            stored_cost: r.get(9)?,
            response_id: r.get(10)?,
            request_id: r.get(11)?,
            attribution,
            parent,
            compaction: r
                .get::<_, Option<String>>(16)?
                .and_then(|s| serde_json::from_str(&s).ok()),
        })
    })?;
    rows.collect()
}

pub(crate) fn price_basis(table: &[(String, [f64; 4])]) -> Value {
    let serialized = serde_json::to_vec(
        &json!({"rates":table,"claude_fallback_rates":token_ledger::PRICE_DEFAULT}),
    )
    .expect("finite configured prices");
    json!({"label":COST_BASIS,"currency":"USD","unit":"per_million_tokens","version":format!("sha256:{:x}",Sha256::digest(serialized)),"as_of":Value::Null,"as_of_status":"not_verified","rates":table,"claude_fallback_rates":token_ledger::PRICE_DEFAULT,"billed_usd":Value::Null,"billing_measured":false,"billing_status":"unknown"})
}

#[derive(Default)]
struct Aggregate {
    cost: f64,
    tokens: i64,
    turns: i64,
    unpriced: i64,
}
impl Aggregate {
    fn add(&mut self, e: &Entry, table: &[(String, [f64; 4])]) {
        self.cost += token_ledger::turn_cost_usd(table, &e.model, e.tokens);
        self.tokens += e.tokens.iter().sum::<i64>();
        self.turns += 1;
        self.unpriced += i64::from(!token_ledger::model_is_priced(table, &e.model));
    }
    fn value(&self) -> Value {
        json!({"cost":self.cost,"tokens":self.tokens,"turns":self.turns,"unpriced_turns":self.unpriced,"cost_status":if self.unpriced>0 {"partial"}else{"priced"},"cost_basis":COST_BASIS,"billed_usd":Value::Null,"billing_measured":false})
    }
}

/// Pure over a database snapshot, explicit scope and price configuration.
pub(crate) fn summarize(
    conn: &Connection,
    scope: &Scope,
    table: &[(String, [f64; 4])],
) -> rusqlite::Result<Value> {
    let entries = entries(conn, scope, None)?;
    let mut total = Aggregate::default();
    let mut delegated = Aggregate::default();
    let mut groups: BTreeMap<&str, BTreeMap<String, Aggregate>> = BTreeMap::new();
    let mut labels: BTreeMap<String, Value> = BTreeMap::new();
    let mut aliases: BTreeMap<String, Value> = BTreeMap::new();
    let mut st = conn.prepare("SELECT id,display_name,name_aliases FROM usage_workers")?;
    for row in st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (id, name, old) = row?;
        aliases.insert(id,json!({"current_alias":name,"historical_aliases":serde_json::from_str::<Value>(&old).unwrap_or(json!([]))}));
    }
    let mut titles = BTreeMap::new();
    let mut st = conn.prepare("SELECT id,title FROM issues")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (id, title) = row?;
        titles.insert(id, title);
    }
    let mut cache = 0i64;
    let mut context = 0i64;
    let mut peak_context = 0i64;
    let mut attributed = 0i64;
    let mut no_identity = 0i64;
    let mut unpriced_stored = 0.0;
    let mut free_stored = 0.0;
    let mut free_turns = 0i64;
    for e in &entries {
        total.add(e, table);
        if e.parent.is_some() || e.conversation.starts_with("agent-") {
            delegated.add(e, table);
        }
        cache += e.tokens[1];
        let ctx = e.tokens[..3].iter().sum::<i64>();
        context += ctx;
        peak_context = peak_context.max(ctx);
        attributed += i64::from(e.attribution.primary.is_some());
        no_identity += i64::from(e.attribution.worker_id.is_none());
        if !token_ledger::model_is_priced(table, &e.model) {
            unpriced_stored += e.stored_cost;
        }
        if token_ledger::model_rate(table, &e.model).is_some_and(|r| r.iter().all(|v| *v == 0.0)) {
            free_turns += 1;
            free_stored += e.stored_cost;
        }
        let link = e.attribution.primary.as_ref();
        let work = link
            .map(|p| {
                format!(
                    "{}:{}",
                    serde_json::to_value(&p.kind).unwrap().as_str().unwrap(),
                    p.id
                )
            })
            .unwrap_or_default();
        if let Some(p) = link {
            labels.insert(work.clone(),json!({"work_reference":p,"title":titles.get(&p.id).cloned().unwrap_or_else(||p.id.clone()),"work_kind":p.kind,"work_id":p.id}));
        }
        let worker = e
            .attribution
            .worker_id
            .as_ref()
            .map(|id| format!("id:{id}"))
            .unwrap_or_else(|| format!("alias:{}", e.session));
        let day = chrono::DateTime::from_timestamp(e.ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        for (group, key) in [
            ("by_session", worker),
            ("by_work_reference", work),
            ("by_model", e.model.clone()),
            ("by_day", day),
            ("by_conversation", e.conversation.clone()),
            ("by_attribution_reason", e.attribution.reason.clone()),
            ("by_attribution_source", e.attribution.source.clone()),
        ] {
            groups
                .entry(group)
                .or_default()
                .entry(key)
                .or_default()
                .add(e, table);
        }
    }
    let mut result = json!({"measured":true,"n_considered":total.turns,"since":scope.since,"until":scope.until,"until_exclusive":true,"timezone":"UTC","day_timezone":"UTC","cost_basis":price_basis(table),"billed_usd":Value::Null,"billing_measured":false,
        "session":scope.session,"worker_id":scope.worker_id,"unknown_worker":scope.unknown_worker,"total_cost":total.cost,"total_tokens":total.tokens,"total_turns":total.turns,"cost_status":if total.unpriced>0 {"partial"}else{"priced"},"unmetered_turns":total.unpriced,"unpriced_turns":total.unpriced,"unmetered_cost_in_ledger":unpriced_stored,"free_turns":free_turns,"free_cost_in_ledger":free_stored,
        "cache_hit_pct": if context>0 {(cache as f64*1000.0/context as f64).round()/10.0}else{0.0},"delegated_cost":delegated.cost,"delegated_turns":delegated.turns,"delegated_tokens":delegated.tokens,"delegated_included_in_total":true,
        "attribution_coverage":{"measured":true,"responses":total.turns,"attributed_responses":attributed,"unattributed_responses":total.turns-attributed,"no_worker_identity":no_identity,"no_work_reference":total.turns-attributed,"percent":if total.turns>0 {Some(attributed as f64*100.0/total.turns as f64)}else{None}},
        "context":{"measured":true,"definition":"input + cache_read + cache_write per response","peak_tokens":peak_context},"compaction":{"measured":entries.iter().any(|e|e.compaction.is_some()),"boundaries":entries.iter().filter(|e|e.compaction.is_some()).count(),"unknown_reason":"only explicit retained transcript boundaries are evidence"}});
    for (group, rows) in groups {
        let mut rows: Vec<_> = rows
            .into_iter()
            .map(|(key, a)| {
                let mut v = a.value();
                match group {
                    "by_session" => {
                        let id = key.strip_prefix("id:");
                        let legacy_alias = key.strip_prefix("alias:").unwrap_or("");
                        let recorded: Vec<_> = entries
                            .iter()
                            .filter(|e| match id {
                                Some(id) => e.attribution.worker_id.as_deref() == Some(id),
                                None => {
                                    e.attribution.worker_id.is_none() && e.session == legacy_alias
                                }
                            })
                            .collect();
                        v["session"] =
                            json!(recorded.first().map(|e| e.session.as_str()).unwrap_or(""));
                        v["worker_id"] = json!(id);
                        let info = id.and_then(|id| aliases.get(id));
                        v["current_alias"] = info
                            .map(|v| v["current_alias"].clone())
                            .unwrap_or(Value::Null);
                        let mut history: std::collections::BTreeSet<String> = recorded
                            .iter()
                            .filter(|e| !e.session.is_empty())
                            .map(|e| e.session.clone())
                            .collect();
                        if let Some(old) = info.and_then(|v| v["historical_aliases"].as_array()) {
                            history
                                .extend(old.iter().filter_map(|v| v.as_str()).map(str::to_owned));
                        }
                        v["historical_aliases"] = json!(history);
                    }
                    "by_work_reference" => {
                        v["title"] = json!("Unattributed work");
                        v["task"] = json!("");
                        v["key"] = json!(key);
                        if let Some(info) = labels.get(&key) {
                            if info["work_kind"] == "card" {
                                v["task"] = info["work_id"].clone();
                            }
                            for (k, val) in info.as_object().unwrap() {
                                v[k] = val.clone();
                            }
                        }
                    }
                    "by_model" => {
                        v["model"] = json!(key);
                        v["priced"] = json!(token_ledger::model_is_priced(table, &key));
                    }
                    "by_day" => v["day"] = json!(key),
                    "by_conversation" => v["conversation"] = json!(key),
                    _ => v["key"] = json!(key),
                }
                v
            })
            .collect();
        if group != "by_day" {
            rows.sort_by(|a, b| {
                b["cost"]
                    .as_f64()
                    .unwrap_or_default()
                    .total_cmp(&a["cost"].as_f64().unwrap_or_default())
            });
        }
        result[group] = json!(rows);
    }
    for name in [
        "by_session",
        "by_work_reference",
        "by_model",
        "by_day",
        "by_conversation",
        "by_attribution_reason",
        "by_attribution_source",
    ] {
        if result[name].is_null() {
            result[name] = json!([]);
        }
    }
    result["by_task"] = result["by_work_reference"].clone();
    result["unmetered_models"] = json!(result["by_model"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["priced"] == false)
        .collect::<Vec<_>>());
    let fresh: Option<i64> =
        conn.query_row("SELECT MAX(ts) FROM token_ledger", [], |r| r.get(0))?;
    result["ledger_fresh_through"] = json!(fresh);
    result["ledger_stale_s"] = json!(fresh.map(|t| (chrono::Utc::now().timestamp() - t).max(0)));
    Ok(result)
}

fn error(status: StatusCode, message: String) -> Response {
    (
        status,
        Json(json!({"error":message,"measured":false,"n_considered":0})),
    )
        .into_response()
}
async fn rollup(State(state): State<AppState>, RawQuery(q): RawQuery) -> Response {
    let (scope, days, group) =
        match scope(q.as_deref().unwrap_or(""), chrono::Utc::now().timestamp()) {
            Ok(s) => s,
            Err(e) => return error(StatusCode::BAD_REQUEST, e),
        };
    let table = token_ledger::prices(&crate::config::amux_home());
    let result = state
        .store
        .read()
        .and_then(|c| summarize(&c, &scope, &table).map_err(Into::into));
    match result {
        Ok(mut v) => {
            v["days"] = json!(days);
            v["session"] = json!(scope.session);
            v["group"] = json!(group);
            Json(v).into_response()
        }
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    }
}
async fn ledger(State(state): State<AppState>, RawQuery(q): RawQuery) -> Response {
    let query = q.as_deref().unwrap_or("");
    let (scope, _, _) = match scope(query, chrono::Utc::now().timestamp()) {
        Ok(s) => s,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    let p = parse_qs(query);
    let parse = |k, default| {
        qs_get(&p, k)
            .map(|v| v.parse::<i64>().map_err(|_| format!("invalid {k}")))
            .unwrap_or(Ok(default))
    };
    let (after, limit) = match (parse("after_id", 0), parse("limit", 100)) {
        (Ok(a), Ok(l)) if a >= 0 && (1..=MAX_PAGE_SIZE).contains(&l) => (a, l),
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "after_id must be nonnegative; limit must be 1..500".into(),
            )
        }
    };
    let conn = match state.store.read() {
        Ok(c) => c,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    };
    let mut rows = match entries(&conn, &scope, Some((after, limit))) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    };
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    let table = token_ledger::prices(&crate::config::amux_home());
    let evidence:Vec<_>=rows.iter().map(|r|json!({"id":r.id,"ts":r.ts,"worker_id":r.attribution.worker_id,"historical_alias":r.session,"conversation":r.conversation,"response_id":r.response_id,"request_id":r.request_id,"parent_conversation":r.parent,"model":r.model,"tokens":r.tokens,"context_tokens":r.tokens[..3].iter().sum::<i64>(),"compaction":{"measured":r.compaction.is_some(),"evidence":r.compaction},"attribution":r.attribution,"cost":token_ledger::turn_cost_usd(&table,&r.model,r.tokens),"priced":token_ledger::model_is_priced(&table,&r.model),"billed_usd":Value::Null,"billing_measured":false})).collect();
    Json(json!({"measured":true,"n_considered":rows.len(),"since":scope.since,"until":scope.until,"until_exclusive":true,"timezone":"UTC","cost_basis":price_basis(&table),"rows":evidence,"next_after_id":next})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Connection {
        let c = crate::db::migrate::test_memdb();
        c.execute("INSERT INTO _amux_workers(id,display_name,name_aliases,created_at,updated_at) VALUES('wrk_a','current','[\"old\"]','2026-01-01','2026-01-01')",[]).unwrap();
        c.execute(
            "INSERT INTO issues(id,title,created,updated) VALUES('A','Live title',1,1)",
            [],
        )
        .unwrap();
        for (id, session, worker, conv, model, tokens, ts) in [
            (1, "old", Some("wrk_a"), "conv", "opus", 1000, 100),
            (2, "current", Some("wrk_a"), "agent-child", "", 2000, 101),
            (3, "", None, "unknown", "qwen", 3000, 102),
            (4, "", None, "outside", "opus", 99, 200),
        ] {
            let a = json!({"worker_id":worker,"primary":if worker.is_some(){json!({"kind":"card","id":"A","source":"task_attempts","effective_at":99})}else{Value::Null},"attempt":1,"generation":2,"reason":if worker.is_some(){"attributed"}else{"no_worker_identity"},"source":"historical_intervals","effective_at":99});
            c.execute("INSERT INTO token_ledger(id,ts,session,conversation,model,input,output,cost_usd,worker_id,attribution,task,message_id) VALUES(?1,?2,?3,?4,?5,?6,10,999,?7,?8,?9,?10)",rusqlite::params![id,ts,session,conv,model,tokens,worker,a.to_string(),if worker.is_some(){"A"}else{""},format!("response-{id}")]).unwrap();
        }
        c
    }
    #[test]
    fn totals_and_every_breakdown_use_the_same_prices_including_empty_models() {
        let c = fixture();
        let scope = Scope {
            since: 100,
            until: 200,
            ..Default::default()
        };
        let d = summarize(&c, &scope, &token_ledger::default_prices()).unwrap();
        assert_eq!(d["total_turns"], 3);
        assert_eq!(d["total_tokens"], 6030);
        assert_eq!(d["unpriced_turns"], 1);
        assert_eq!(d["free_turns"], 1);
        assert!((d["total_cost"].as_f64().unwrap() - 0.01575).abs() < 1e-10);
        for group in [
            "by_session",
            "by_work_reference",
            "by_model",
            "by_day",
            "by_conversation",
            "by_attribution_reason",
            "by_attribution_source",
        ] {
            let sum: f64 = d[group]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["cost"].as_f64().unwrap())
                .sum();
            assert!(
                (sum - d["total_cost"].as_f64().unwrap()).abs() < 1e-10,
                "{group}"
            );
        }
        assert!(d["billed_usd"].is_null());
        assert_eq!(d["billing_measured"], false);
        assert_eq!(d["timezone"], "UTC");
        assert_eq!(d["attribution_coverage"]["attributed_responses"], 2);
        assert_eq!(d["by_session"][0]["worker_id"], "wrk_a");
        assert_eq!(d["by_session"][0]["current_alias"], "current");
        assert_eq!(d["by_work_reference"][0]["title"], "Live title");
        assert_eq!(d["compaction"]["measured"], false);
    }
    #[test]
    fn ledger_filters_exact_boundaries_unknown_identity_work_and_pagination() {
        let c = fixture();
        let base = Scope {
            since: 100,
            until: 200,
            ..Default::default()
        };
        let rows = entries(&c, &base, Some((0, 1))).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, 1);
        assert_eq!(entries(&c, &base, Some((2, 1))).unwrap()[0].id, 3);
        assert_eq!(
            entries(
                &c,
                &Scope {
                    unknown_worker: true,
                    ..base.clone()
                },
                None
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            entries(
                &c,
                &Scope {
                    worker_id: "wrk_a".into(),
                    ..base.clone()
                },
                None
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            entries(
                &c,
                &Scope {
                    conversation: "conv".into(),
                    ..base.clone()
                },
                None
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            entries(
                &c,
                &Scope {
                    work_kind: "card".into(),
                    work_id: "A".into(),
                    ..base.clone()
                },
                None
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            entries(
                &c,
                &Scope {
                    unattributed: true,
                    ..base.clone()
                },
                None
            )
            .unwrap()
            .len(),
            1
        );
        assert!(entries(
            &c,
            &Scope {
                members: Some(vec![]),
                ..base
            },
            None
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn invalid_scope_is_machine_checkable_and_blank_session_remains_fleet() {
        for q in [
            "since=200&until=100",
            "days=garbage",
            "unknown_worker=maybe",
            "unknown_worker=true&session=old",
            "work_kind=pr",
            "work_kind=bad&work_id=x",
        ] {
            assert!(scope(q, 200).is_err(), "{q}");
        }
        let (s, _, _) = scope("session=&unknown_worker=true", 200).unwrap();
        assert!(s.unknown_worker);
        assert!(s.session.is_empty());
    }
}
