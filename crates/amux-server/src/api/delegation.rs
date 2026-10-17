// Owner-delegated configuration changes: the ONE allowlist that answers "may X do Y to Z" — refs maxtongwang/ai-config#160.
// Every config-mutating endpoint asks `authorize` (or the #8 wrappers built on `decide`); anything not on the table is refused.
//! Max, 2026-10-17: "Why? Automation is key and losing it is not the way even
//! for security reasons". amux PR #8 made worker creation, membership and peer
//! config owner-only. That is safe, but it stalled automation. This module
//! reopens delegated paths under one explicit, verified and audited table.
//!
//! WHO (`Class`, from the one verified identity `session_verbs::worker_origin`):
//!
//! | Class | Who |
//! |---|---|
//! | Owner | no worker identity: the owner bearer, the dashboard, a verified human member |
//! | SelfWorker | a worker acting on itself (its Chat tab counts as itself) |
//! | Mrpopo | the verified worker named `mrpopo`, Max's owner agent |
//! | Lead { team } | a verified worker with `CC_ORCHESTRATOR=1` and group `leads`; `team` is its team tag that the target shares |
//! | Worker | every other worker, and any claim that failed verification |
//!
//! WHAT (`decide`, the table; default deny):
//!
//! | Class | Allowed | Needs max_ref |
//! |---|---|---|
//! | Owner | everything | never |
//! | Lead T | create workers tagged only T; rename/stop workers in T; remove T from a worker in T; rules of group T; model/effort on workers in T | any other tag change it makes |
//! | Mrpopo | nothing without proof | rules and membership of `leads`; routing; permission toggles on any worker |
//! | SelfWorker | non-privileged settings ([`key_class`] SelfOk/LeadOk), rename, stop | never; privileged keys stay refused |
//! | Worker | nothing | never |
//!
//! Secrets ([`is_secret_key`]) are Max-only: no row grants them, with or
//! without max_ref. A request carries an optional `max_ref: {session, time}`
//! (verified by `max_ref::verify_max_ref`) and an optional `why`; every change a
//! non-owner makes through the table is appended to the delegated-change log
//! (`delegation_log::record`) by [`Grant::commit`]. Refusals are not logged as
//! changes: each is a WARN line with `verdict = "delegation_refused"`.
use super::AppState;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// The verified worker name that acts on Max's behalf.
pub(crate) const MRPOPO: &str = "mrpopo";
use super::session_verbs::LEADS_GROUP;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Owner,
    Lead { team: String },
    Mrpopo,
    SelfWorker,
    Worker,
}

impl Class {
    /// The log's `class` field: "lead:<team>" | "mrpopo" | "self" (owner and
    /// worker never reach the log).
    pub(crate) fn label(&self) -> String {
        match self {
            Class::Owner => "owner".into(),
            Class::Lead { team } => format!("lead:{team}"),
            Class::Mrpopo => "mrpopo".into(),
            Class::SelfWorker => "self".into(),
            Class::Worker => "worker".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// `owner_only` names something only the owner grants at create (the chat
    /// worker type, a store group id).
    CreateWorker { tags: Vec<String>, owner_only: Option<&'static str> },
    RenameWorker { to: String },
    StopWorker,
    /// A worker's whole tag set, before and after (normalized).
    SetTags { from: Vec<String>, to: Vec<String> },
    SetGroupRules { group: String },
    /// Setting names: config-body fields as sent, env keys as `env:KEY`, scope
    /// capabilities as `scope:<cap>`, worker-row fields as `row:<field>`.
    SetConfig { keys: Vec<String> },
    /// A store group id or the chat worker type on an existing worker.
    SetMembership { group: String },
    CreateGroup { group: String },
    SetRouting,
    SetSecret { key: String },
}

impl Action {
    pub(crate) fn label(&self) -> String {
        match self {
            Action::CreateWorker { .. } => "create_worker".into(),
            Action::RenameWorker { .. } => "rename_worker".into(),
            Action::StopWorker => "stop_worker".into(),
            Action::SetTags { .. } => "set_tags".into(),
            Action::SetGroupRules { .. } => "set_group_rules".into(),
            Action::SetConfig { keys } => format!("set_config:{}", keys.join(",")),
            Action::SetMembership { group } => format!("set_membership:{group}"),
            Action::CreateGroup { .. } => "create_group".into(),
            Action::SetRouting => "set_routing".into(),
            Action::SetSecret { .. } => "set_secret".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decision {
    Allow,
    AllowWithMaxRef,
    Deny { code: &'static str, reason: String },
}

/// What one setting is, for [`decide`]. Exhaustive over the config PATCH body
/// (`session_verbs::CONFIG_PATCH_OPS` plus its modifiers): an unknown name is
/// `OwnerOnly`, so a new field is refused until someone classifies it here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyClass {
    /// A worker may set it on itself.
    SelfOk,
    /// SelfOk, and a lead may set it on a worker in its team.
    LeadOk,
    /// Widens what the worker may do without the owner: mrpopo with max_ref.
    Permission,
    /// Cross-group message routing: mrpopo with max_ref.
    Routing,
    /// Messaging authority (groups, worker type): handled as membership.
    Membership,
    Secret,
    /// The owner's, never delegated.
    OwnerOnly,
}

/// Request fields that carry delegation metadata, never a setting.
pub(crate) const META_FIELDS: [&str; 2] = ["max_ref", "why"];

/// Config-body fields, one row per field (`config_patch_with_liveness`).
const CONFIG_FIELDS: &[(&str, KeyClass)] = &[
    ("model", KeyClass::LeadOk),
    ("effort", KeyClass::LeadOk),
    // Rides with `model` (overrides the unknown-model check).
    ("force", KeyClass::LeadOk),
    ("provider", KeyClass::SelfOk),
    ("dir", KeyClass::SelfOk),
    ("task_summary", KeyClass::SelfOk),
    ("desc", KeyClass::SelfOk),
    ("toggle_pin", KeyClass::SelfOk),
    ("branch", KeyClass::SelfOk),
    ("create", KeyClass::SelfOk),
    ("worktree_profile", KeyClass::SelfOk),
    ("worktree_base", KeyClass::SelfOk),
    ("mcp", KeyClass::SelfOk),
    ("new_conversation", KeyClass::SelfOk),
    ("restart", KeyClass::SelfOk),
    ("rename", KeyClass::SelfOk),
    // The commit guard PATCH verb, and the instructions/memory verbs.
    ("commit-guard", KeyClass::SelfOk),
    ("instructions", KeyClass::SelfOk),
    ("memory", KeyClass::SelfOk),
    // The verification command the harness runs as this worker's gate.
    ("worktree_verify", KeyClass::OwnerOnly),
    ("worker_type", KeyClass::Membership),
    ("tags", KeyClass::Membership),
    ("toggle_yolo", KeyClass::Permission),
    ("toggle_auto_continue", KeyClass::Permission),
    ("external_email_allowed", KeyClass::Permission),
    ("isolated", KeyClass::Permission),
    ("send_allow", KeyClass::Routing),
    ("spans_groups", KeyClass::Routing),
    // Scope capabilities on a worker layer (`/api/scope`).
    ("scope:memory", KeyClass::SelfOk),
    ("scope:skin", KeyClass::SelfOk),
    ("scope:status_mode", KeyClass::SelfOk),
    ("scope:gates", KeyClass::Permission),
    ("scope:connectors", KeyClass::Permission),
    ("scope:rules", KeyClass::OwnerOnly),
    // Worker-row fields (`PATCH /api/workers/{id}`).
    ("row:display_name", KeyClass::SelfOk),
    ("row:name", KeyClass::SelfOk),
    ("row:cwd", KeyClass::SelfOk),
    ("row:provider", KeyClass::SelfOk),
    ("row:model", KeyClass::LeadOk),
    ("row:backend", KeyClass::SelfOk),
    ("row:expect_version", KeyClass::SelfOk),
    ("row:environment", KeyClass::OwnerOnly),
    ("row:permissions", KeyClass::Permission),
    ("row:group", KeyClass::Membership),
    ("row:worker_type", KeyClass::Membership),
];

/// Env keys that widen what a worker may do without the owner (beyond
/// `scope::is_owner_only_env_key`): the permission-bypass flags live in
/// CC_FLAGS, and CC_ORCHESTRATOR is half of being a lead.
const PERMISSION_ENV_KEYS: [&str; 4] = ["CC_FLAGS", "CC_AUTO_CONTINUE", "CC_ISOLATED", "CC_ORCHESTRATOR"];

/// A key name that holds a secret: never delegable, with or without max_ref.
pub(crate) fn is_secret_key(key: &str) -> bool {
    let k = key.trim().to_ascii_uppercase();
    k == super::worker_identity::TOKEN_ENV
        || k.starts_with("AMUX_CONTRACT_")
        || super::worker_identity::credential_like(&k)
}

pub(crate) fn key_class(key: &str) -> KeyClass {
    if let Some(env) = key.strip_prefix("env:") {
        let k = env.trim().to_ascii_uppercase();
        if is_secret_key(&k) {
            return KeyClass::Secret;
        }
        if k == "CC_TAGS" || k == "CC_WORKER_TYPE" {
            return KeyClass::Membership;
        }
        if k == "CC_SEND_ALLOW" || k == super::worker_messaging::OPEN_INBOUND_KEY {
            return KeyClass::Routing;
        }
        if k == "CC_WORKTREE_VERIFY" {
            return KeyClass::OwnerOnly;
        }
        if PERMISSION_ENV_KEYS.contains(&k.as_str()) || super::scope::is_owner_only_env_key(&k) {
            return KeyClass::Permission;
        }
        return KeyClass::SelfOk;
    }
    CONFIG_FIELDS.iter().find(|(k, _)| *k == key).map(|(_, c)| *c).unwrap_or(KeyClass::OwnerOnly)
}

fn deny(code: &'static str, reason: String) -> Decision {
    Decision::Deny { code, reason }
}

fn set(v: &[String]) -> BTreeSet<String> {
    v.iter().map(|t| super::session_verbs::norm_label(t)).filter(|t| !t.is_empty()).collect()
}

/// THE table. Pure: identity is resolved by [`classify`], the target's team by
/// [`target_team`]. `target_team` is `Some(team)` only when the target belongs
/// to the lead's `team`.
pub(crate) fn decide(class: &Class, action: &Action, target_team: Option<&str>) -> Decision {
    use Decision::{Allow, AllowWithMaxRef};
    if *class == Class::Owner {
        return Allow;
    }
    let secret = match action {
        Action::SetSecret { key } => Some(key.clone()),
        Action::SetConfig { keys } => keys.iter().find(|k| key_class(k) == KeyClass::Secret).cloned(),
        _ => None,
    };
    if let Some(k) = secret {
        return deny("secret_owner_only", format!("'{k}' holds a secret; setting it is Max's alone and is never delegated"));
    }
    match (class, action) {
        (Class::SelfWorker, Action::SetConfig { keys }) => match keys.iter().find(|k| !matches!(key_class(k), KeyClass::SelfOk | KeyClass::LeadOk)) {
            None => Allow,
            Some(k) if key_class(k) == KeyClass::Membership => membership_denied(k),
            Some(k) => deny("owner_only_key", format!(
                "'{k}' is owner-only: it widens what this worker may do without the owner. A worker changes only non-privileged settings on itself")),
        },
        (Class::SelfWorker, Action::RenameWorker { .. } | Action::StopWorker) => Allow,
        (Class::SelfWorker, Action::SetTags { .. } | Action::SetMembership { .. }) => membership_denied("tags"),
        (Class::Lead { team }, Action::CreateWorker { tags, owner_only }) => {
            let outside: Vec<String> = set(tags).into_iter().filter(|t| t != team).collect();
            match owner_only {
                Some(o) => deny("worker_create_tags_outside_team", format!("A lead may create workers only inside its own team: {o} is the owner's to grant.")),
                None if outside.is_empty() => Allow,
                None => deny("worker_create_tags_outside_team", format!(
                    "A lead may create workers only inside its own team: tags {outside:?} are outside the lead's team '{team}' ('{LEADS_GROUP}' is the owner's to grant).")),
            }
        }
        (Class::Lead { team }, Action::RenameWorker { .. } | Action::StopWorker | Action::SetConfig { .. }) if target_team != Some(team.as_str()) => {
            deny("peer_config_owner_only", format!("a lead acts only on workers in its own team '{team}'; this worker is not in it"))
        }
        (Class::Lead { .. }, Action::RenameWorker { .. } | Action::StopWorker) => Allow,
        (Class::Lead { team }, Action::SetConfig { keys }) => match keys.iter().find(|k| key_class(k) != KeyClass::LeadOk) {
            None => Allow,
            Some(k) => deny("peer_config_owner_only", format!(
                "a lead may set only model and effort on workers in its team '{team}', not '{k}'")),
        },
        (Class::Lead { team }, Action::SetTags { from, to }) => {
            let (from, to) = (set(from), set(to));
            let changed: BTreeSet<String> = from.symmetric_difference(&to).cloned().collect();
            if from.contains(team) && changed.iter().all(|t| t == team) {
                Allow
            } else {
                AllowWithMaxRef
            }
        }
        (Class::Lead { team }, Action::SetGroupRules { group }) if group == team => Allow,
        (Class::Mrpopo, Action::SetGroupRules { group } | Action::SetMembership { group }) if group == LEADS_GROUP => AllowWithMaxRef,
        (Class::Mrpopo, Action::SetTags { from, to }) => {
            let changed: BTreeSet<String> = set(from).symmetric_difference(&set(to)).cloned().collect();
            if changed.iter().all(|t| t == LEADS_GROUP) {
                AllowWithMaxRef
            } else {
                deny("delegation_not_allowed", format!("mrpopo changes only '{LEADS_GROUP}' membership; {changed:?} is another team's"))
            }
        }
        (Class::Mrpopo, Action::SetRouting) => AllowWithMaxRef,
        (Class::Mrpopo, Action::SetConfig { keys }) if keys.iter().all(|k| matches!(key_class(k), KeyClass::Permission | KeyClass::Routing)) => AllowWithMaxRef,
        (Class::Worker | Class::Mrpopo, Action::CreateWorker { .. }) => {
            deny("worker_create_requires_lead", format!("Only leads can create workers; the caller is not a lead (CC_ORCHESTRATOR=1 and the '{LEADS_GROUP}' group)."))
        }
        (_, Action::SetTags { .. } | Action::SetMembership { .. }) => membership_denied("tags"),
        (_, Action::RenameWorker { .. } | Action::StopWorker | Action::SetConfig { .. }) => {
            deny("peer_config_owner_only", "configuring another worker is the owner's; no delegation row grants it to this caller".into())
        }
        (_, other) => deny("delegation_not_allowed", format!("no delegation row lets a {} do {}", class.label(), other.label())),
    }
}

fn membership_denied(what: &str) -> Decision {
    deny("worker_group_membership_refused", format!("Group membership changes require direct owner input ('{what}')."))
}

/// What the request acts on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Worker(String),
    Group(String),
    NewWorker,
    Fleet,
}

impl Target {
    fn name(&self) -> String {
        match self {
            Target::Worker(w) => w.clone(),
            Target::Group(g) => format!("group:{g}"),
            Target::NewWorker => "new-worker".into(),
            Target::Fleet => "fleet".into(),
        }
    }
}

/// Whether `origin` is a lead: CC_ORCHESTRATOR=1 and in `leads`. Returns its
/// team tags (groups other than `leads`), or None for a non-lead.
fn lead_teams(origin: &str) -> Option<BTreeSet<String>> {
    let groups = super::session_verbs::lane_groups(origin);
    let orchestrator = super::session_verbs::parse_env(origin).get("CC_ORCHESTRATOR").is_some_and(|v| v.trim() == "1");
    (orchestrator && groups.contains(LEADS_GROUP)).then(|| groups.into_iter().filter(|g| g != LEADS_GROUP).collect())
}

fn target_groups(target: &Target, action: &Action) -> BTreeSet<String> {
    match (target, action) {
        (_, Action::CreateWorker { tags, .. }) => set(tags),
        (Target::Worker(w), Action::SetTags { from, .. }) if !from.is_empty() || super::session_verbs::lane_groups(w).is_empty() => set(from),
        (Target::Worker(w), _) => super::session_verbs::lane_groups(w),
        (Target::Group(g), _) => [super::session_verbs::norm_label(g)].into_iter().collect(),
        _ => BTreeSet::new(),
    }
}

/// The class of `origin` (the verified worker identity, empty for the owner)
/// acting on `target`. A lead's `team` is its team tag the target shares, else
/// its first team tag (empty when it has none).
pub(crate) fn classify(origin: &str, target: &Target, action: &Action) -> Class {
    if origin.is_empty() {
        return Class::Owner;
    }
    if origin == super::worker_identity::UNVERIFIED_IDENTITY {
        return Class::Worker;
    }
    if let Target::Worker(w) = target {
        if origin == w || super::chat_worker::companion_parent(origin) == Some(w.as_str()) {
            return Class::SelfWorker;
        }
    }
    if origin == MRPOPO {
        return Class::Mrpopo;
    }
    match lead_teams(origin) {
        Some(teams) => {
            let tg = target_groups(target, action);
            let team = teams.iter().find(|t| tg.contains(*t)).or_else(|| teams.iter().next()).cloned().unwrap_or_default();
            Class::Lead { team }
        }
        None => Class::Worker,
    }
}

/// `Some(team)` when the target belongs to the lead's team.
pub(crate) fn target_team(class: &Class, target: &Target, action: &Action) -> Option<String> {
    let Class::Lead { team } = class else { return None };
    (!team.is_empty() && target_groups(target, action).contains(team)).then(|| team.clone())
}

/// Classify and decide for a request's headers, with no max_ref handling:
/// for the #8 wrappers in `session_verbs`, whose callers have no `AppState`.
/// `AllowWithMaxRef` is returned as is; such callers treat it as a refusal.
pub(crate) fn decide_for(headers: &HeaderMap, target: &Target, action: &Action) -> (String, Class, Decision) {
    let origin = super::session_verbs::worker_origin(headers);
    let class = classify(&origin, target, action);
    let tt = target_team(&class, target, action);
    let decision = decide(&class, action, tt.as_deref());
    (origin, class, decision)
}

/// The #8 refusal shape: `{"ok": false, "code", "error"}`.
pub(crate) fn refusal_body(code: &str, error: &str) -> Value {
    json!({"ok": false, "code": code, "error": error})
}

fn refuse(origin: &str, class: &Class, target: &Target, action: &Action, code: &str, error: String) -> Response {
    tracing::warn!(actor = %origin, class = %class.label(), target = %target.name(), action = %action.label(), code,
        verdict = "delegation_refused", measured = true, n_considered = 1, "a delegated change was refused");
    (StatusCode::FORBIDDEN, Json(refusal_body(code, &error))).into_response()
}

/// Proof that a non-owner change passed the table. [`Grant::commit`] appends
/// it, with before/after, to the delegated-change log once the change landed.
#[derive(Clone, Debug)]
pub(crate) struct Grant {
    pub actor: String,
    pub class: Class,
    pub action: String,
    pub target: String,
    pub max_ref: Option<super::max_ref::MaxRef>,
    pub evidence_row: Option<i64>,
    pub why: Option<String>,
}

impl Grant {
    pub(crate) fn commit(&self, state: &AppState, before: Value, after: Value) {
        let change = super::delegation_log::DelegatedChange {
            ts: crate::config::now_f64(),
            actor: self.actor.clone(),
            class: self.class.label(),
            action: self.action.clone(),
            target: self.target.clone(),
            before,
            after,
            max_ref: self.max_ref.clone(),
            evidence_row: self.evidence_row,
            why: self.why.clone(),
        };
        if let Err(e) = super::delegation_log::record(state, change) {
            tracing::warn!(actor = %self.actor, action = %self.action, target = %self.target, error = %e,
                verdict = "delegated_change_log_failed", measured = true, n_considered = 1,
                "a delegated change landed but its log entry could not be written");
        }
    }
}

/// Parse the optional `max_ref` and `why` fields of a request body. A
/// malformed `max_ref` is reported, never read as absent.
fn request_meta(body: Option<&Value>) -> Result<(Option<super::max_ref::MaxRef>, Option<String>), String> {
    let Some(body) = body else { return Ok((None, None)) };
    let max_ref = match body.get("max_ref") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value::<super::max_ref::MaxRef>(v.clone())
            .map_err(|e| format!("max_ref must be {{\"session\": <worker>, \"time\": <unix seconds>}}: {e}"))?),
    };
    let why = body.get("why").and_then(Value::as_str).map(|s| s.trim().chars().take(500).collect::<String>()).filter(|s| !s.is_empty());
    Ok((max_ref, why))
}

/// THE gate for a mutating request. `Ok(None)`: the owner, nothing to log.
/// `Ok(Some(grant))`: a non-owner allowed through the table; call
/// [`Grant::commit`] after the change lands. `Err(response)`: refused, in the
/// #8 shape (`max_ref_*` codes name the missing proof).
pub(crate) fn authorize(state: &AppState, headers: &HeaderMap, target: &Target, action: &Action, body: Option<&Value>) -> Result<Option<Grant>, Response> {
    let (origin, class, decision) = decide_for(headers, target, action);
    if class == Class::Owner {
        return Ok(None);
    }
    let (max_ref, why) = request_meta(body).map_err(|e| (StatusCode::BAD_REQUEST, Json(refusal_body("max_ref_invalid", &e))).into_response())?;
    let evidence_row = match decision {
        Decision::Deny { code, reason } => return Err(refuse(&origin, &class, target, action, code, reason)),
        Decision::Allow => None,
        Decision::AllowWithMaxRef => {
            let names = max_ref_targets(target);
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            match super::max_ref::verify_max_ref(state, max_ref.as_ref(), &names, crate::config::now_f64()) {
                Ok(ev) => Some(ev.row_id),
                Err(r) => {
                    let error = format!("{} needs proof of Max's instruction (max_ref: {{session, time}} naming {}): {}",
                        action.label(), names.join(" or "), r.message());
                    return Err(refuse(&origin, &class, target, action, r.code(), error));
                }
            }
        }
    };
    tracing::info!(actor = %origin, class = %class.label(), target = %target.name(), action = %action.label(),
        evidence_row = ?evidence_row, verdict = "delegation_allowed", measured = true, n_considered = 1,
        "a non-owner change passed the delegation table");
    Ok(Some(Grant { actor: origin, class, action: action.label(), target: target.name(), max_ref, evidence_row, why }))
}

/// The names Max's instruction must mention for a change to `target`.
fn max_ref_targets(target: &Target) -> Vec<String> {
    match target {
        Target::Worker(w) => vec![w.clone()],
        Target::Group(g) => vec![g.clone()],
        Target::NewWorker | Target::Fleet => vec![],
    }
}

/// A worker env file as a JSON object (for before/after; the log redacts).
pub(crate) fn env_snapshot(worker: &str) -> Value {
    let env = super::session_verbs::parse_env(worker);
    Value::Object(env.pairs().iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect())
}

/// Only the keys that differ between two snapshots, as (before, after).
pub(crate) fn changed_only(before: &Value, after: &Value) -> (Value, Value) {
    let (Some(b), Some(a)) = (before.as_object(), after.as_object()) else { return (before.clone(), after.clone()) };
    let keys: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
    let (mut ob, mut oa) = (serde_json::Map::new(), serde_json::Map::new());
    for k in keys.into_iter().filter(|k| b.get(*k) != a.get(*k)) {
        ob.insert(k.clone(), b.get(k).cloned().unwrap_or(Value::Null));
        oa.insert(k.clone(), a.get(k).cloned().unwrap_or(Value::Null));
    }
    (Value::Object(ob), Value::Object(oa))
}

/// The action a config PATCH body asks for (one setting per PATCH is enforced
/// by the handler; every named field is classified here regardless).
pub(crate) fn config_action(target: &str, body: &Value) -> Action {
    if let Some(v) = body.get("tags") {
        let to = super::session_verbs::tags_of(Some(v)).unwrap_or_else(|_| vec!["!invalid".into()]);
        let from: Vec<String> = super::session_verbs::lane_groups(target).into_iter().collect();
        return Action::SetTags { from, to };
    }
    if let Some(to) = body.get("rename").and_then(Value::as_str) {
        return Action::RenameWorker { to: to.trim().to_string() };
    }
    if body.get("send_allow").is_some() || body.get("spans_groups").is_some() {
        return Action::SetRouting;
    }
    let keys: Vec<String> = body.as_object().map(|o| o.keys().filter(|k| !META_FIELDS.contains(&k.as_str())).cloned().collect()).unwrap_or_default();
    Action::SetConfig { keys }
}

/// A non-owner may not give a worker the name the mrpopo class keys on.
pub(crate) fn rename_refusal(headers: &HeaderMap, to: &str) -> Option<Response> {
    let origin = super::session_verbs::worker_origin(headers);
    if origin.is_empty() || super::session_verbs::norm_label(to) != MRPOPO {
        return None;
    }
    Some(refuse(&origin, &Class::Worker, &Target::Worker(to.into()), &Action::RenameWorker { to: to.into() },
        "rename_reserved_identity", format!("'{MRPOPO}' is the owner agent's identity; only the owner may give a worker that name")))
}

/// PATCH config, gated and logged: `PATCH /api/sessions/<w>/config` and
/// `PATCH /api/workers/{id}/config` both come here.
pub(crate) async fn guarded_config_patch(state: &AppState, headers: &HeaderMap, name: &str, body: &Value) -> Response {
    let action = config_action(name, body);
    if let Action::RenameWorker { to } = &action {
        if let Some(r) = rename_refusal(headers, to) {
            return r;
        }
    }
    let target = Target::Worker(name.to_string());
    let grant = match authorize(state, headers, &target, &action, Some(body)) {
        Ok(g) => g,
        Err(r) => return r,
    };
    let before = env_snapshot(name);
    let resp = super::session_verbs::config_patch(state, name, body).await;
    if let Some(g) = grant.filter(|_| resp.status().is_success()) {
        let (b, a) = match &action {
            Action::RenameWorker { to } => (json!({"name": name}), json!({"name": to})),
            _ => changed_only(&before, &env_snapshot(name)),
        };
        g.commit(state, b, a);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead(t: &str) -> Class {
        Class::Lead { team: t.into() }
    }
    fn cfg(keys: &[&str]) -> Action {
        Action::SetConfig { keys: keys.iter().map(|k| k.to_string()).collect() }
    }
    fn tags(from: &[&str], to: &[&str]) -> Action {
        Action::SetTags { from: from.iter().map(|s| s.to_string()).collect(), to: to.iter().map(|s| s.to_string()).collect() }
    }
    fn code(d: Decision) -> &'static str {
        match d {
            Decision::Deny { code, .. } => code,
            Decision::Allow => "allow",
            Decision::AllowWithMaxRef => "max_ref",
        }
    }

    #[test]
    fn delegation_owner_may_do_anything() {
        for a in [cfg(&["toggle_yolo"]), Action::SetSecret { key: "GITHUB_TOKEN".into() }, tags(&[], &["leads"]), Action::SetRouting, Action::CreateGroup { group: "x".into() }] {
            assert_eq!(decide(&Class::Owner, &a, None), Decision::Allow, "{a:?}");
        }
    }

    #[test]
    fn delegation_lead_same_team_allowed_other_team_refused() {
        let l = lead("dev");
        assert_eq!(code(decide(&l, &Action::CreateWorker { tags: vec!["dev".into()], owner_only: None }, None)), "allow");
        assert_eq!(code(decide(&l, &Action::CreateWorker { tags: vec!["leads".into()], owner_only: None }, None)), "worker_create_tags_outside_team");
        assert_eq!(code(decide(&l, &Action::CreateWorker { tags: vec!["ops".into()], owner_only: None }, None)), "worker_create_tags_outside_team");
        assert_eq!(code(decide(&l, &Action::CreateWorker { tags: vec!["dev".into()], owner_only: Some("the chat worker type") }, None)), "worker_create_tags_outside_team");
        for a in [Action::RenameWorker { to: "x".into() }, Action::StopWorker, cfg(&["model"]), cfg(&["effort"])] {
            assert_eq!(code(decide(&l, &a, Some("dev"))), "allow", "{a:?}");
            assert_eq!(code(decide(&l, &a, None)), "peer_config_owner_only", "other team {a:?}");
        }
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "dev".into() }, None)), "allow");
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "ops".into() }, None)), "delegation_not_allowed");
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "leads".into() }, None)), "delegation_not_allowed");
    }

    #[test]
    fn delegation_lead_may_not_change_a_team_workers_non_model_field() {
        let l = lead("dev");
        for k in ["desc", "dir", "toggle_yolo", "external_email_allowed", "isolated", "worktree_verify", "env:CC_MODEL", "mcp"] {
            assert_eq!(code(decide(&l, &cfg(&[k]), Some("dev"))), "peer_config_owner_only", "{k}");
        }
    }

    #[test]
    fn delegation_lead_tags_within_team_free_widening_needs_max_ref() {
        let l = lead("dev");
        // Removing T from a team worker narrows: allowed.
        assert_eq!(code(decide(&l, &tags(&["dev", "infra"], &["infra"]), Some("dev"))), "allow");
        assert_eq!(code(decide(&l, &tags(&["dev"], &["dev"]), Some("dev"))), "allow");
        // Leads, another team, or pulling a worker in: membership change.
        assert_eq!(code(decide(&l, &tags(&["dev"], &["dev", "leads"]), Some("dev"))), "max_ref");
        assert_eq!(code(decide(&l, &tags(&["dev"], &["dev", "ops"]), Some("dev"))), "max_ref");
        assert_eq!(code(decide(&l, &tags(&["ops"], &["ops", "dev"]), None)), "max_ref");
        assert_eq!(code(decide(&l, &tags(&["dev", "ops"], &["dev"]), Some("dev"))), "max_ref");
    }

    #[test]
    fn delegation_mrpopo_needs_max_ref_and_only_for_leads_routing_and_toggles() {
        let m = Class::Mrpopo;
        assert_eq!(code(decide(&m, &Action::SetGroupRules { group: "leads".into() }, None)), "max_ref");
        assert_eq!(code(decide(&m, &Action::SetMembership { group: "leads".into() }, None)), "max_ref");
        assert_eq!(code(decide(&m, &tags(&["dev"], &["dev", "leads"]), None)), "max_ref");
        assert_eq!(code(decide(&m, &Action::SetRouting, None)), "max_ref");
        for k in ["toggle_yolo", "external_email_allowed", "isolated", "env:CC_FLAGS", "env:AMUX_EMAIL_EXTERNAL_ALLOW"] {
            assert_eq!(code(decide(&m, &cfg(&[k]), None)), "max_ref", "{k}");
        }
        assert_eq!(code(decide(&m, &tags(&["dev"], &["dev", "ops"]), None)), "delegation_not_allowed");
        assert_eq!(code(decide(&m, &Action::SetGroupRules { group: "dev".into() }, None)), "delegation_not_allowed");
        assert_eq!(code(decide(&m, &cfg(&["model"]), None)), "peer_config_owner_only");
        assert_eq!(code(decide(&m, &Action::CreateWorker { tags: vec![], owner_only: None }, None)), "worker_create_requires_lead");
    }

    #[test]
    fn delegation_self_non_privileged_only() {
        let s = Class::SelfWorker;
        for k in ["model", "effort", "desc", "task_summary", "env:CC_MODEL", "scope:memory"] {
            assert_eq!(code(decide(&s, &cfg(&[k]), None)), "allow", "{k}");
        }
        // ai-config #160's gap: these were 200 on self before this table.
        for k in ["external_email_allowed", "toggle_yolo", "toggle_auto_continue", "isolated", "worktree_verify",
            "env:CC_FLAGS", "env:AMUX_EMAIL_EXTERNAL_ALLOW", "env:AMUX_NEEDS_INPUT_AUTO_OUTBOUND", "env:CC_ORCHESTRATOR",
            "send_allow", "scope:gates", "scope:rules", "row:environment", "an_unclassified_field"] {
            assert_eq!(code(decide(&s, &cfg(&[k]), None)), "owner_only_key", "{k}");
        }
        assert_eq!(code(decide(&s, &cfg(&["worker_type"]), None)), "worker_group_membership_refused");
        assert_eq!(code(decide(&s, &tags(&["dev"], &["dev", "leads"]), None)), "worker_group_membership_refused");
    }

    #[test]
    fn delegation_other_worker_refused_everything() {
        let w = Class::Worker;
        assert_eq!(code(decide(&w, &cfg(&["model"]), None)), "peer_config_owner_only");
        assert_eq!(code(decide(&w, &Action::StopWorker, None)), "peer_config_owner_only");
        assert_eq!(code(decide(&w, &Action::CreateWorker { tags: vec![], owner_only: None }, None)), "worker_create_requires_lead");
        assert_eq!(code(decide(&w, &tags(&[], &["dev"]), None)), "worker_group_membership_refused");
        assert_eq!(code(decide(&w, &Action::SetGroupRules { group: "dev".into() }, None)), "delegation_not_allowed");
        assert_eq!(code(decide(&w, &Action::SetRouting, None)), "delegation_not_allowed");
    }

    #[test]
    fn delegation_secrets_are_never_delegable() {
        for c in [lead("dev"), Class::Mrpopo, Class::SelfWorker, Class::Worker] {
            for a in [Action::SetSecret { key: "X".into() }, cfg(&["env:GITHUB_TOKEN"]), cfg(&["env:AMUX_WORKER_TOKEN"]),
                cfg(&["env:AMUX_CONTRACT_DONE"]), cfg(&["env:OPENAI_API_KEY"]), cfg(&["model", "env:DB_PASSWORD"])] {
                assert_eq!(code(decide(&c, &a, Some("dev"))), "secret_owner_only", "{c:?} {a:?}");
            }
        }
    }

    #[test]
    fn delegation_every_config_patch_field_is_classified() {
        // The sweep: each field config_patch reads has a row, so a new field
        // falls to OwnerOnly only because nobody classified it, and fails here.
        for k in super::super::session_verbs::config_patch_fields_for_delegation() {
            assert!(CONFIG_FIELDS.iter().any(|(f, _)| *f == k), "config PATCH field '{k}' has no delegation row");
        }
    }
}
