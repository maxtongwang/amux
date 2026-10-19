// Owner-delegated configuration changes: the ONE allowlist that answers "may X do Y to Z" — refs maxtongwang/ai-config#160.
// Every config-mutating endpoint asks `authorize` (or `refusal_value` where no AppState exists); anything not in the table is refused.
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
//! | SelfWorker | a worker acting on itself (its Chat tab counts only when process attribution agrees) |
//! | Mrpopo | the verified worker named `mrpopo`, Max's owner agent |
//! | Lead | a verified worker with `CC_ORCHESTRATOR=1` and group `leads`; its teams are its other groups |
//! | Worker | every other worker, and any claim that failed verification |
//!
//! WHAT (`decide`; default deny):
//!
//! | Class | Allowed | Through a grant Max approves |
//! |---|---|---|
//! | Owner | everything | - |
//! | Lead | create workers tagged with a non-empty subset of its teams, writing only SelfOk/LeadOk settings; rename (not to a reserved name), stop and lifecycle verbs on non-lead workers in its team; remove its team tag from such a worker; rules and group config of its teams; model and effort on such workers | nothing |
//! | Mrpopo | nothing | each setting in [`GRANT_ROWS`] and the action rows in `decide_mrpopo`: stored as a pending grant, applied only on Max's approval (api/delegation_grants.rs) |
//! | SelfWorker | settings in [`SELF_ENV_KEYS`] / SelfOk config fields, rename (not to a reserved name), stop and lifecycle verbs | nothing |
//! | Worker | nothing | nothing |
//!
//! Never delegated: secret values ([`is_secret_key`]) and everything in
//! [`HELD_FOR_MAX`] (one line each, so delegating one later is a one-line
//! change). A lead rename keeps the worker's token and old-name alias (amux
//! #3); the rule matches amux #40 (ai-config #203): non-lead, same team, and
//! neither the target nor the new name is mrpopo.
//! Every non-owner change is logged by [`Grant::commit`]; refusals are WARN
//! lines with `verdict = "delegation_refused"`, never log entries.
use super::AppState;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// The verified worker name that acts on Max's behalf.
pub(crate) const MRPOPO: &str = "mrpopo";
use super::session_verbs::LEADS_GROUP;

/// HANDS-ONLY: owner only, never delegable, not even through a grant (dev-lead,
/// 2026-10-17). Secret VALUES are the first class ([`is_secret_key`]); the
/// rest are one line each, with the reason, so changing one is a one-line
/// edit. `env:` entries are key prefixes, `name:` entries reserved worker
/// names, `mrpopo:own-config` mrpopo changing its own settings.
pub const HELD_FOR_MAX: &[(&str, &str)] = &[
    ("env:AMUX_CONTRACT_", "the rule-10 token-check switch: turning it off un-verifies a worker's identity"),
    ("env:AMUX_PANE_PROMPT_HISTORY", "the recorder typed grant approvals are read from: turning it off blinds the proof"),
    ("env:AMUX_NEEDS_INPUT_AUTO_MONEY", "the auto-approve spending switch and money cap: paying is Max's move"),
    ("env:AMUX_CARD_MAX_USD", "a card's spending limit: paying is Max's move"),
    ("env:AMUX_DELEGATION_APPROVAL", "the delegation approval mode (cite or passkey): who may approve a widening change is the owner's call"),
    ("name:mrpopo", "the owner agent's identity: a worker given this name would act as mrpopo"),
    ("name:owner", "reads as the owner in every log and message"),
    ("mrpopo:own-config", "a worker widening itself, directly or through a group or global layer it reads, would need no one else's act"),
];

fn held_items(prefix: &str) -> impl Iterator<Item = &'static str> + '_ {
    HELD_FOR_MAX.iter().filter_map(move |(h, _)| h.strip_prefix(prefix))
}

fn held_reason(item: &str) -> &'static str {
    HELD_FOR_MAX.iter().find(|(h, _)| *h == item).map(|(_, r)| *r).unwrap_or("")
}

fn held_env(key: &str) -> bool {
    held_items("env:").any(|p| key.starts_with(p))
}

/// A worker name only the owner may give (create, rename, row rename).
pub(crate) fn reserved_name(name: &str) -> bool {
    let n = super::session_verbs::norm_label(name);
    held_items("name:").any(|r| r == n)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Owner,
    /// `team` is the lead's team the target shares (else its first team);
    /// `teams` is every team tag it holds.
    Lead { team: String, teams: Vec<String> },
    Mrpopo,
    SelfWorker,
    Worker,
}

impl Class {
    /// The log's `class` field: "lead:<team>" | "mrpopo" | "self".
    pub(crate) fn label(&self) -> String {
        match self {
            Class::Owner => "owner".into(),
            Class::Lead { team, .. } => format!("lead:{team}"),
            Class::Mrpopo => "mrpopo".into(),
            Class::SelfWorker => "self".into(),
            Class::Worker => "worker".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// `settings`: every setting the create writes besides name and tags
    /// (`create:<field>`, `row:<field>` or `env:<KEY>`). `owner_only` names
    /// what only the owner grants at create (chat type, store group id).
    CreateWorker { tags: Vec<String>, settings: Vec<String>, owner_only: Option<&'static str> },
    RenameWorker { to: String },
    StopWorker,
    /// start, reset, clear, git, worktree, archive, keys.
    Lifecycle { verb: String },
    /// A worker's whole tag set, before and after.
    SetTags { from: Vec<String>, to: Vec<String> },
    SetGroupRules { group: String },
    /// department/goal/kpis (`PATCH /api/groups/<g>/config`).
    SetGroupConfig { group: String },
    /// Setting names: config-body fields as sent, env keys `env:KEY`, scope
    /// capabilities `scope:<cap>`, row fields `row:<field>`, instance settings
    /// `settings:<name>` / `instance:apply`, group/global layers prefixed.
    SetConfig { keys: Vec<String> },
    CreateGroup { group: String },
    SetRouting,
    SetSecret { key: String },
    /// Making a lead in ONE write: CC_ORCHESTRATOR=1 plus the `leads` tag.
    MakeLead,
}

impl Action {
    pub(crate) fn label(&self) -> String {
        match self {
            Action::CreateWorker { .. } => "create_worker".into(),
            Action::RenameWorker { .. } => "rename_worker".into(),
            Action::StopWorker => "stop_worker".into(),
            Action::Lifecycle { verb } => format!("lifecycle:{verb}"),
            Action::SetTags { .. } => "set_tags".into(),
            Action::SetGroupRules { .. } => "set_group_rules".into(),
            Action::SetGroupConfig { .. } => "set_group_config".into(),
            Action::SetConfig { keys } => format!("set_config:{}", keys.join(",")),
            Action::CreateGroup { .. } => "create_group".into(),
            Action::SetRouting => "set_routing".into(),
            Action::SetSecret { .. } => "set_secret".into(),
            Action::MakeLead => "make_lead".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Decision {
    Allow,
    /// Not applied now: stored as a pending grant that applies on Max's
    /// approval (`delegation_grants`).
    NeedsGrant,
    Deny { code: &'static str, reason: String },
}

/// What one setting is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyClass {
    /// A worker may set it on itself (and a lead may write it at create).
    SelfOk,
    /// SelfOk, and a lead may set it on a non-lead worker in its team.
    LeadOk,
    /// Messaging authority: groups, worker type.
    Membership,
    /// Mrpopo through a grant Max approves.
    Grant,
    Secret,
    /// In [`HELD_FOR_MAX`].
    Held,
    /// No row grants it.
    OwnerOnly,
}

pub const GRANT_ROWS: &[&str] = &[
    "toggle_yolo",
    "env:CC_FLAGS",
    "create:yolo",
    "create:flags",
    "external_email_allowed",
    "env:AMUX_EMAIL_EXTERNAL_ALLOW",
    "isolated",
    "env:CC_ISOLATED",
    "create:isolated",
    "toggle_auto_continue",
    "env:CC_AUTO_CONTINUE",
    "env:AMUX_NEEDS_INPUT_AUTO*",
    "env:AMUX_BROWSER_PROFILES_*",
    "env:AMUX_APPROVAL_TYPES",
    "scope:gates",
    "scope:connectors",
    "row:permissions",
    "env:CC_ORCHESTRATOR",
    "send_allow",
    "spans_groups",
    "env:CC_SEND_ALLOW",
    "settings:commit-guard",
    "settings:task-guard",
    "settings:default-model",
    "instance:apply",
    "worktree_verify",
    "env:CC_WORKTREE_VERIFY",
    "env:CC_VERIFY",
    "env:CC_DEPLOY_CHECK",
    "env:CC_WORKTREE_AUTO_MERGE",
    "env:AMUX_YOLO_AUTO_YES",
    "env:CC_PROJECT_LEAD",
    "env:CC_RECEIVE_DENY",
    "create:creator",
    "settings:alerts",
];

/// The plain words that name each grant row in an ask (review B4 ruling: the
/// ask names the setting "in plain words"). An ask names a setting when it
/// contains any one of its words, or the key itself (`CC_FLAGS`,
/// `toggle_yolo`). `every_grant_row_has_plain_words` keeps this exhaustive.
pub const SETTING_WORDS: &[(&str, &[&str])] = &[
    ("toggle_yolo", &["yolo", "skip permissions"]),
    ("env:CC_FLAGS", &["yolo", "flags", "skip permissions"]),
    ("create:yolo", &["yolo"]),
    ("create:flags", &["flags", "yolo"]),
    ("external_email_allowed", &["external email"]),
    ("env:AMUX_EMAIL_EXTERNAL_ALLOW", &["external email"]),
    ("isolated", &["isolated", "isolation", "isolate"]),
    ("env:CC_ISOLATED", &["isolated", "isolation", "isolate"]),
    ("create:isolated", &["isolated", "isolation"]),
    ("toggle_auto_continue", &["auto continue", "autocontinue"]),
    ("env:CC_AUTO_CONTINUE", &["auto continue", "autocontinue"]),
    ("env:AMUX_NEEDS_INPUT_AUTO*", &["auto approve", "auto approval"]),
    ("env:AMUX_BROWSER_PROFILES_*", &["browser profile"]),
    ("env:AMUX_APPROVAL_TYPES", &["approval type"]),
    ("scope:gates", &["gate"]),
    ("scope:connectors", &["connector"]),
    ("row:permissions", &["permission"]),
    ("env:CC_ORCHESTRATOR", &["lead", "orchestrator"]),
    ("send_allow", &["routing", "send allow", "messaging"]),
    ("spans_groups", &["routing", "spans groups", "cross group"]),
    ("env:CC_SEND_ALLOW", &["routing", "send allow", "messaging"]),
    ("settings:commit-guard", &["commit guard"]),
    ("settings:task-guard", &["task guard"]),
    ("settings:default-model", &["default model"]),
    ("instance:apply", &["config apply", "apply config", "instance config", "column", "skin"]),
    ("worktree_verify", &["verify command", "verify"]),
    ("env:CC_WORKTREE_VERIFY", &["verify command", "verify"]),
    ("env:CC_VERIFY", &["verify command", "verify"]),
    ("env:CC_DEPLOY_CHECK", &["deploy check"]),
    ("env:CC_WORKTREE_AUTO_MERGE", &["auto merge"]),
    ("env:AMUX_YOLO_AUTO_YES", &["auto yes"]),
    ("env:CC_PROJECT_LEAD", &["project lead"]),
    ("env:CC_RECEIVE_DENY", &["receive deny", "block messages"]),
    ("create:creator", &["creator"]),
    ("settings:alerts", &["alert"]),
];

/// The spellings that name the global layer or the whole instance.
const GLOBAL_WORDS: &[&str] = &["global", "fleet", "every worker", "all workers", "everyone", "instance"];

/// The words naming one setting key: its row's plain words and the key
/// itself without its layer and kind prefixes.
fn setting_words(key: &str) -> Vec<String> {
    let mut k = key;
    while let Some(rest) = k.strip_prefix("group:").or_else(|| k.strip_prefix("global:")) {
        k = rest;
    }
    let mut words: Vec<String> = SETTING_WORDS.iter()
        .filter(|(row, _)| match row.strip_suffix('*') { Some(p) => k.starts_with(p), None => *row == k })
        .flat_map(|(_, w)| w.iter().map(|x| x.to_string())).collect();
    let bare = ["env:", "scope:", "settings:", "row:", "create:", "instance:"].iter().find_map(|p| k.strip_prefix(p)).unwrap_or(k);
    words.push(if k == "instance:apply" { "instance apply".into() } else { bare.to_string() });
    words
}

/// What a citation's ask must name for this change (review B4): the target
/// and every setting, each in any one of its spellings.
pub(crate) fn cite_terms(target: &Target, action: &Action) -> super::max_ref::CiteTerms {
    let target_words = match target {
        Target::Worker(w) => vec![w.clone()],
        Target::Group(g) => vec![g.clone()],
        Target::NewWorker => vec!["new worker".into()],
        Target::Fleet | Target::Settings(_) => GLOBAL_WORDS.iter().map(|w| w.to_string()).collect(),
    };
    let settings: Vec<Vec<String>> = match action {
        Action::SetConfig { keys } => keys.iter().map(|k| setting_words(k)).collect(),
        Action::SetTags { from, to } => set(from).symmetric_difference(&set(to)).map(|g| vec![g.clone()]).collect(),
        Action::MakeLead => vec![vec!["lead".into()]],
        Action::SetGroupRules { .. } => vec![vec!["rule".into()]],
        Action::SetGroupConfig { .. } => vec![["config", "configuration", "department", "goal", "kpi"].iter().map(|w| w.to_string()).collect()],
        Action::CreateGroup { .. } => vec![vec!["group".into()]],
        Action::SetRouting => vec![["routing", "inbound", "cross group"].iter().map(|w| w.to_string()).collect()],
        other => vec![vec![other.label()]],
    };
    super::max_ref::CiteTerms { target: target_words, settings }
}

/// Env keys a worker may set on itself (H4: an explicit list, not "anything
/// not privileged"). Display and runtime-choice keys only.
pub const SELF_ENV_KEYS: &[&str] = &[
    "CC_MODEL", "CC_EFFORT", "CC_DESC", "CC_DIR", "CC_PINNED", "CC_BRANCH", "CC_PROVIDER", "CC_WORKTREE_PROFILE", "CC_MCP",
];

/// Request fields that carry delegation metadata, never a setting.
pub(crate) const META_FIELDS: [&str; 2] = ["cite", "why"];

/// Config-body, scope, row and create fields that are not grant rows.
const FIELDS: &[(&str, KeyClass)] = &[
    ("model", KeyClass::LeadOk),
    ("effort", KeyClass::LeadOk),
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
    ("commit-guard", KeyClass::SelfOk),
    ("instructions", KeyClass::SelfOk),
    ("memory", KeyClass::SelfOk),
    ("keys", KeyClass::SelfOk),
    ("worker_type", KeyClass::Membership),
    ("tags", KeyClass::Membership),
    // The board-automation switches (review B1): the owner's, listed so the
    // config PATCH sweep names them rather than leaving them unclassified.
    ("auto_drain_backlog", KeyClass::OwnerOnly),
    ("board_auto_pickup", KeyClass::OwnerOnly),
    ("board_auto_continue", KeyClass::OwnerOnly),
    ("board_standing_orders", KeyClass::OwnerOnly),
    ("board_decompose", KeyClass::OwnerOnly),
    ("board_force_adherence", KeyClass::OwnerOnly),
    ("scope:memory", KeyClass::SelfOk),
    ("scope:skin", KeyClass::SelfOk),
    ("scope:status_mode", KeyClass::SelfOk),
    ("scope:rules", KeyClass::OwnerOnly),
    ("row:display_name", KeyClass::SelfOk),
    ("row:name", KeyClass::SelfOk),
    ("row:cwd", KeyClass::SelfOk),
    ("row:provider", KeyClass::SelfOk),
    ("row:model", KeyClass::LeadOk),
    ("row:backend", KeyClass::SelfOk),
    ("row:environment", KeyClass::OwnerOnly),
    ("row:group", KeyClass::Membership),
    ("row:worker_type", KeyClass::Membership),
    ("create:dir", KeyClass::SelfOk),
    ("create:desc", KeyClass::SelfOk),
    ("create:model", KeyClass::LeadOk),
    ("create:provider", KeyClass::SelfOk),
    ("create:prompt", KeyClass::SelfOk),
    ("create:worktree", KeyClass::SelfOk),
    ("create:worktree_profile", KeyClass::SelfOk),
];

/// A key name that holds a secret value: never delegable, not even through a grant.
/// THE secret predicate: decide() and the log's redactor both use it (review
/// M4), the union of the identity report's and `/api/system-jobs`' name tests.
pub(crate) fn is_secret_key(key: &str) -> bool {
    let k = key.trim().to_ascii_uppercase();
    k == super::worker_identity::TOKEN_ENV
        || super::worker_identity::credential_like(&k)
        || crate::runtime_jobs::registry::is_secret_env(&k)
}

fn grant_row(name: &str) -> bool {
    GRANT_ROWS.iter().any(|row| match row.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => *row == name,
    })
}

pub(crate) fn key_class(key: &str) -> KeyClass {
    // Group and global layers: secrets and held keys keep their class; no
    // other row exists for them.
    // Group and global layers: secrets, held keys and mrpopo's explicit rows
    // keep their class (a row's phrase applies on every layer); a worker's
    // own or a lead's settings are not settable there by anyone but the owner.
    for layer in ["group:", "global:"] {
        if let Some(inner) = key.strip_prefix(layer) {
            return match key_class(inner) {
                c @ (KeyClass::Secret | KeyClass::Held | KeyClass::Grant) => c,
                _ => KeyClass::OwnerOnly,
            };
        }
    }
    if let Some(env) = key.strip_prefix("env:") {
        let k = env.trim().to_ascii_uppercase();
        if is_secret_key(&k) {
            return KeyClass::Secret;
        }
        if held_env(&k) {
            return KeyClass::Held;
        }
        if k == "CC_TAGS" || k == "CC_WORKER_TYPE" {
            return KeyClass::Membership;
        }
        if grant_row(&format!("env:{k}")) {
            return KeyClass::Grant;
        }
        if k == "CC_MODEL" || k == "CC_EFFORT" {
            return KeyClass::LeadOk;
        }
        if SELF_ENV_KEYS.contains(&k.as_str()) {
            return KeyClass::SelfOk;
        }
        return KeyClass::OwnerOnly;
    }
    if grant_row(key) {
        return KeyClass::Grant;
    }
    FIELDS.iter().find(|(k, _)| *k == key).map(|(_, c)| *c).unwrap_or(KeyClass::OwnerOnly)
}

fn deny(code: &'static str, reason: String) -> Decision {
    Decision::Deny { code, reason }
}

fn set(v: &[String]) -> BTreeSet<String> {
    v.iter().map(|t| super::session_verbs::norm_label(t)).filter(|t| !t.is_empty()).collect()
}

/// Facts about the target that the table needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TargetFacts {
    /// The target belongs to the lead's `team`.
    pub in_team: bool,
    /// The target is mrpopo or a member of `leads` (a lead never acts on one).
    pub privileged: bool,
    /// The target is the actor itself.
    pub is_actor: bool,
    /// The change reaches the actor: it targets the actor, or a layer the
    /// actor reads (a group it is in, the global layer, an instance-wide
    /// setting). Review B3: a layer write is a write to every worker it
    /// affects.
    pub covers_actor: bool,
}

/// THE self-privilege rule, for a change that reaches its own actor through a
/// layer (review B3): a worker may change on itself only the SelfOk/LeadOk
/// settings, so any other setting, a membership change or routing on a layer
/// that covers the actor is refused as hands-only. Group rules and group
/// config are the team's text, not authority, and stay with the table.
fn widens_self(action: &Action) -> Option<String> {
    let inner = |k: &str| -> String {
        let mut k = k;
        while let Some(rest) = k.strip_prefix("group:").or_else(|| k.strip_prefix("global:")) {
            k = rest;
        }
        k.to_string()
    };
    match action {
        Action::SetConfig { keys } => keys.iter().find(|k| !matches!(key_class(&inner(k)), KeyClass::SelfOk | KeyClass::LeadOk)).cloned(),
        Action::SetTags { .. } => Some("tags".into()),
        Action::MakeLead => Some("make lead".into()),
        Action::SetRouting => Some("routing".into()),
        Action::SetSecret { key } => Some(format!("env:{key}")),
        _ => None,
    }
}

fn membership_denied(what: &str) -> Decision {
    deny("worker_group_membership_refused", format!("Group membership changes require direct owner input ('{what}')."))
}

/// THE table. Pure: identity from [`classify`], target facts from [`facts`].
pub(crate) fn decide(class: &Class, action: &Action, t: &TargetFacts) -> Decision {
    use Decision::Allow;
    if *class == Class::Owner {
        return Allow;
    }
    let keys: Vec<String> = match action {
        Action::SetConfig { keys } => keys.clone(),
        Action::CreateWorker { settings, .. } => settings.clone(),
        Action::SetSecret { key } => vec![format!("env:{key}")],
        _ => vec![],
    };
    if matches!(action, Action::SetSecret { .. }) || keys.iter().any(|k| key_class(k) == KeyClass::Secret) {
        return deny("secret_owner_only", "setting a secret value is Max's alone and is never delegated".into());
    }
    if let Some(k) = keys.iter().find(|k| key_class(k) == KeyClass::Held) {
        let key = k.rsplit("env:").next().unwrap_or(k).to_ascii_uppercase();
        let why = HELD_FOR_MAX.iter().find(|(h, _)| h.strip_prefix("env:").is_some_and(|p| key.starts_with(p))).map(|(_, r)| *r).unwrap_or("");
        return deny("held_for_max", format!("'{k}' is hands-only (HELD_FOR_MAX: {why}); only the owner may change it"));
    }
    if let Action::SetConfig { keys } = action {
        if keys.is_empty() {
            return deny("delegation_not_allowed", "a change that names no setting is not delegable".into());
        }
    }
    // A layer that covers the actor (review B3): its own group, the global
    // layer, an instance-wide setting. Widening yourself through a layer is
    // the same act as widening yourself directly. SelfWorker already decides
    // by this rule, and a plain Worker is refused below.
    if t.covers_actor && matches!(class, Class::Mrpopo | Class::Lead { .. }) {
        if let Some(k) = widens_self(action) {
            return deny("held_for_max", format!(
                "'{k}' on a layer that includes the caller widens the caller itself (HELD_FOR_MAX: {}); only the owner may change it",
                held_reason("mrpopo:own-config")));
        }
    }
    match class {
        Class::Owner => Allow,
        Class::SelfWorker => decide_self(action),
        Class::Lead { team, teams } => decide_lead(team, teams, action, t),
        Class::Mrpopo => decide_mrpopo(action, t),
        Class::Worker => decide_worker(action),
    }
}

fn decide_self(action: &Action) -> Decision {
    match action {
        Action::SetConfig { keys } => match keys.iter().find(|k| !matches!(key_class(k), KeyClass::SelfOk | KeyClass::LeadOk)) {
            None => Decision::Allow,
            Some(k) if key_class(k) == KeyClass::Membership => membership_denied(k),
            Some(k) => deny("owner_only_key", format!(
                "'{k}' is owner-only: it widens what this worker may do without the owner. A worker changes only non-privileged settings on itself")),
        },
        Action::RenameWorker { to } if reserved_name(to) => reserved_denied(to),
        Action::RenameWorker { .. } | Action::StopWorker | Action::Lifecycle { .. } => Decision::Allow,
        Action::SetTags { .. } | Action::MakeLead => membership_denied("make lead"),
        other => deny("delegation_not_allowed", format!("no delegation row lets a worker do {} on itself", other.label())),
    }
}

fn reserved_denied(name: &str) -> Decision {
    deny("reserved_name", format!("'{name}' is a reserved identity; only the owner may give a worker that name"))
}

fn decide_lead(team: &str, teams: &[String], action: &Action, t: &TargetFacts) -> Decision {
    let on_team_worker = t.in_team && !t.privileged;
    let not_team = || deny("peer_config_owner_only", format!(
        "a lead acts only on non-lead workers in its own team '{team}'; this worker is not one"));
    match action {
        Action::CreateWorker { tags, settings, owner_only } => {
            let teams: BTreeSet<String> = teams.iter().cloned().collect();
            let tags = set(tags);
            if let Some(o) = owner_only {
                return deny("worker_create_tags_outside_team", format!("A lead may create workers only inside its own team: {o} is the owner's to grant."));
            }
            if tags.is_empty() {
                return deny("worker_create_tags_outside_team", format!("A lead's new worker must carry its team tag (one of {teams:?}); untagged creates are the owner's."));
            }
            let outside: Vec<&String> = tags.iter().filter(|g| !teams.contains(*g)).collect();
            if !outside.is_empty() {
                return deny("worker_create_tags_outside_team", format!(
                    "A lead may create workers only inside its own team: tags {outside:?} are outside the lead's teams {teams:?} ('{LEADS_GROUP}' is the owner's to grant)."));
            }
            match settings.iter().find(|k| !matches!(key_class(k), KeyClass::SelfOk | KeyClass::LeadOk)) {
                None => Decision::Allow,
                Some(k) => deny("worker_create_setting_refused", format!(
                    "a lead may not create a worker with '{k}'; that setting follows the same rule as on an existing worker")),
            }
        }
        Action::RenameWorker { to } if reserved_name(to) => reserved_denied(to),
        // A "-lead" name grants nothing (lead status comes from groups) but
        // would mislead peers (amux #41, ai-config #205 N2); the owner may.
        Action::RenameWorker { to } if on_team_worker && super::session_verbs::is_lead_suffixed(to) => deny("lead_rename_reserved_suffix",
            format!("a lead may not give a worker in its team '{team}' a name ending in '-lead' ('{to}'); only the owner may")),
        Action::RenameWorker { .. } | Action::StopWorker | Action::Lifecycle { .. } if on_team_worker => Decision::Allow,
        Action::SetConfig { keys } if on_team_worker => match keys.iter().find(|k| key_class(k) != KeyClass::LeadOk) {
            None => Decision::Allow,
            Some(k) => deny("peer_config_owner_only", format!("a lead may set only model and effort on workers in its team '{team}', not '{k}'")),
        },
        Action::RenameWorker { .. } | Action::StopWorker | Action::Lifecycle { .. } | Action::SetConfig { .. } => not_team(),
        Action::SetTags { from, to } => {
            let (from, to) = (set(from), set(to));
            let changed: BTreeSet<String> = from.symmetric_difference(&to).cloned().collect();
            if on_team_worker && from.contains(team) && changed.iter().all(|g| g == team) {
                Decision::Allow
            } else {
                deny("worker_group_membership_refused", format!(
                    "a lead may only remove its team tag '{team}' from a non-lead worker in its team; {changed:?} is a membership change: a grant mrpopo requests, or the owner"))
            }
        }
        Action::SetGroupRules { group } | Action::SetGroupConfig { group } if teams.iter().any(|g| g == group) => Decision::Allow,
        Action::MakeLead => membership_denied("make lead"),
        other => deny("delegation_not_allowed", format!("no delegation row lets a lead do {}", other.label())),
    }
}

/// mrpopo's rows: ONLY the explicit ones ([`GRANT_ROWS`] plus the action
/// rows below). Each becomes a pending grant that applies only on Max's
/// approval. An owner-only key with no explicit row is refused on every layer.
fn decide_mrpopo(action: &Action, t: &TargetFacts) -> Decision {
    if t.is_actor && held_items("mrpopo:").any(|h| h == "own-config") {
        return deny("held_for_max", "mrpopo changing its own configuration is hands-only (HELD_FOR_MAX: mrpopo widening itself would need no one else's act)".into());
    }
    match action {
        Action::SetConfig { keys } => {
            for k in keys {
                match key_class(k) {
                    KeyClass::Grant => {}
                    KeyClass::OwnerOnly => return deny("delegation_not_allowed", format!("'{k}' has no delegation row; it stays the owner's")),
                    KeyClass::SelfOk | KeyClass::LeadOk => {
                        return deny("delegation_not_allowed", format!("'{k}' is the worker's own setting or a lead's; mrpopo has no row for it"))
                    }
                    KeyClass::Membership => return membership_denied(k),
                    KeyClass::Secret | KeyClass::Held => unreachable!("refused in decide"),
                }
            }
            Decision::NeedsGrant
        }
        Action::SetTags { from, to } => {
            if set(from) == set(to) {
                return deny("delegation_not_allowed", "a tag change that changes nothing is not delegable".into());
            }
            Decision::NeedsGrant
        }
        Action::MakeLead | Action::SetGroupRules { .. } | Action::SetGroupConfig { .. } | Action::CreateGroup { .. } | Action::SetRouting => Decision::NeedsGrant,
        Action::CreateWorker { .. } => deny("worker_create_requires_lead", "Only leads can create workers.".into()),
        other => deny("delegation_not_allowed", format!("no delegation row lets mrpopo do {}", other.label())),
    }
}

fn decide_worker(action: &Action) -> Decision {
    match action {
        Action::CreateWorker { .. } => deny("worker_create_requires_lead", format!(
            "Only leads can create workers; the caller is not a lead (CC_ORCHESTRATOR=1 and the '{LEADS_GROUP}' group).")),
        Action::SetTags { .. } | Action::MakeLead => membership_denied("tags"),
        Action::RenameWorker { .. } | Action::StopWorker | Action::Lifecycle { .. } | Action::SetConfig { .. } => {
            deny("peer_config_owner_only", "configuring another worker is the owner's; no delegation row grants it to this caller".into())
        }
        other => deny("delegation_not_allowed", format!("no delegation row lets this worker do {}", other.label())),
    }
}

/// What the request acts on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Target {
    Worker(String),
    Group(String),
    NewWorker,
    Fleet,
    /// Instance-level settings named by the request (columns, skins): the
    /// grant names them.
    Settings(Vec<String>),
}

impl Target {
    fn name(&self) -> String {
        match self {
            Target::Worker(w) => w.clone(),
            Target::Group(g) => format!("group:{g}"),
            Target::NewWorker => "new-worker".into(),
            Target::Fleet => "fleet".into(),
            Target::Settings(names) => format!("settings:{}", names.join(",")),
        }
    }
}

/// A lead's team tags (groups other than `leads`), or None for a non-lead.
fn lead_teams(origin: &str) -> Option<Vec<String>> {
    let groups = super::session_verbs::lane_groups(origin);
    let orchestrator = super::session_verbs::parse_env(origin).get("CC_ORCHESTRATOR").is_some_and(|v| v.trim() == "1");
    (orchestrator && groups.contains(LEADS_GROUP)).then(|| groups.into_iter().filter(|g| g != LEADS_GROUP).collect())
}

fn target_groups(target: &Target, action: &Action) -> BTreeSet<String> {
    match (target, action) {
        (_, Action::CreateWorker { tags, .. }) => set(tags),
        (Target::Worker(w), _) => super::session_verbs::lane_groups(w),
        (Target::Group(g), _) => [super::session_verbs::norm_label(g)].into_iter().collect(),
        _ => BTreeSet::new(),
    }
}

/// The class of `origin` (verified worker identity, empty for the owner)
/// acting on `target`.
pub(crate) fn classify(origin: &str, target: &Target, action: &Action) -> Class {
    if origin.is_empty() {
        return Class::Owner;
    }
    if origin == super::worker_identity::UNVERIFIED_IDENTITY {
        return Class::Worker;
    }
    if origin == MRPOPO {
        return Class::Mrpopo;
    }
    if let Target::Worker(w) = target {
        if origin == w || super::chat_worker::companion_parent(origin) == Some(w.as_str()) {
            return Class::SelfWorker;
        }
    }
    match lead_teams(origin) {
        Some(teams) => {
            let tg = target_groups(target, action);
            let team = teams.iter().find(|t| tg.contains(*t)).or_else(|| teams.first()).cloned().unwrap_or_default();
            Class::Lead { team, teams }
        }
        None => Class::Worker,
    }
}

/// The facts [`decide`] needs about `target` for `class`.
pub(crate) fn facts(origin: &str, class: &Class, target: &Target, action: &Action) -> TargetFacts {
    let groups = target_groups(target, action);
    let (in_team, privileged) = match target {
        Target::Worker(w) => (
            matches!(class, Class::Lead { team, .. } if !team.is_empty() && groups.contains(team)),
            w == MRPOPO || super::session_verbs::lane_groups(w).contains(LEADS_GROUP),
        ),
        _ => (false, false),
    };
    let is_actor = matches!(target, Target::Worker(w) if w == origin);
    let global_key = matches!(action, Action::SetConfig { keys } if keys.iter().any(|k| k.starts_with("global:")));
    let covers_actor = !origin.is_empty() && (is_actor || global_key || match target {
        Target::Group(g) => super::session_verbs::lane_groups(origin).contains(&super::session_verbs::norm_label(g)),
        // The global layer and instance-wide settings reach every worker.
        Target::Fleet | Target::Settings(_) => true,
        Target::Worker(_) | Target::NewWorker => false,
    });
    TargetFacts { in_team, privileged, is_actor, covers_actor }
}

/// The request's verified worker identity, with M12 applied: a Chat-tab
/// identity (`X@chat`) counts only when process attribution says the request
/// came from that tab or from X; otherwise it is an unverified claim.
///
/// N13: a worker claim gets delegated power only when proven, by process
/// attribution or the lane's valid rule-10 token (`worker_identity::enforce`
/// stamps the proof); a header-only claim on a lane whose rule 10 is off or
/// unminted is an unverified claim here.
pub(crate) fn verified_origin(headers: &HeaderMap) -> String {
    let origin = super::session_verbs::worker_origin(headers);
    if !origin.is_empty() && origin != super::worker_identity::UNVERIFIED_IDENTITY && !super::worker_identity::identity_proven(headers, &origin) {
        tracing::info!(claimed = %origin, verdict = "delegation_identity_unproven", measured = true, n_considered = 1,
            "a worker claim without token or process proof gets no delegated power");
        return super::worker_identity::UNVERIFIED_IDENTITY.to_string();
    }
    if let Some(parent) = super::chat_worker::companion_parent(&origin) {
        let attributed = headers.get(super::process_identity::PROCESS_WORKER_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
        if attributed != origin && attributed != parent {
            return super::worker_identity::UNVERIFIED_IDENTITY.to_string();
        }
    }
    origin
}

/// Classify and decide for a request's headers, with no grant handling.
pub(crate) fn decide_for(headers: &HeaderMap, target: &Target, action: &Action) -> (String, Class, Decision) {
    let origin = verified_origin(headers);
    // A worker acting on ITSELF is not configuring another worker (dev-lead,
    // #18). An unproven claim still reaches only decide_self's non-privileged
    // set, as on main; on any other target it stays unverified.
    let claimed = super::session_verbs::worker_origin(headers);
    let self_claim = origin == super::worker_identity::UNVERIFIED_IDENTITY
        && !claimed.is_empty()
        && claimed != super::worker_identity::UNVERIFIED_IDENTITY
        && matches!(target, Target::Worker(w) if *w == claimed);
    // Forced, not classified: an unproven "mrpopo" or lead claim on itself
    // must not pick up that class's powers.
    let (origin, class) = if self_claim { (claimed, Class::SelfWorker) } else { let c = classify(&origin, target, action); (origin, c) };
    let decision = decide(&class, action, &facts(&origin, &class, target, action));
    (origin, class, decision)
}

/// The #8 refusal shape: `{"ok": false, "code", "error"}`.
pub(crate) fn refusal_body(code: &str, error: &str) -> Value {
    json!({"ok": false, "code": code, "error": error})
}

fn refused(origin: &str, class: &Class, target: &Target, action: &Action, code: &str, error: &str) -> Value {
    tracing::warn!(actor = %origin, class = %class.label(), target = %target.name(), action = %action.label(), code,
        verdict = "delegation_refused", measured = true, n_considered = 1, "a delegated change was refused");
    refusal_body(code, error)
}

/// The decision as an optional #8-shaped 403 body, for callers with no
/// `AppState` (so no grant can be stored: `NeedsGrant` is refused here,
/// naming the endpoints that can store one).
pub(crate) fn refusal_value(headers: &HeaderMap, target: &Target, action: &Action) -> Option<Value> {
    let (origin, class, decision) = decide_for(headers, target, action);
    let (code, error) = match decision {
        Decision::Allow => return None,
        Decision::NeedsGrant => ("grant_unavailable", format!(
            "{} needs Max's approval as a grant; send it through its own endpoint (PATCH /api/sessions/<worker>/config, PUT /api/scope, PATCH /api/groups/<g>/config), which stores the grant", action.label())),
        Decision::Deny { code, reason } => (code, reason),
    };
    Some(refused(&origin, &class, target, action, code, &error))
}

/// Proof that a non-owner change passed the table. [`Grant::commit`] appends
/// it, with before/after, to the delegated-change log once the change landed.
#[derive(Clone, Debug)]
pub(crate) struct Grant {
    pub actor: String,
    pub class: Class,
    pub action: String,
    pub target: String,
    /// The approved grant this change applies, if any.
    pub grant: Option<String>,
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
            grant: self.grant.clone(),
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

/// The optional `why` of a request body.
fn request_why(body: Option<&Value>) -> Option<String> {
    body?.get("why").and_then(Value::as_str).map(|s| s.trim().chars().take(500).collect::<String>()).filter(|s| !s.is_empty())
}

/// THE gate for a mutating request. `Ok(None)`: the owner, nothing to log.
/// `Ok(Some(grant))`: allowed through the table; call [`Grant::commit`] after
/// the change lands. `Err(response)`: refused in the #8 shape.
#[allow(clippy::result_large_err)] // the refusal IS the handler's response; it is returned as-is
pub(crate) fn authorize(state: &AppState, headers: &HeaderMap, target: &Target, action: &Action, body: Option<&Value>) -> Result<Option<Grant>, Response> {
    let (origin, class, decision) = decide_for(headers, target, action);
    if class == Class::Owner {
        return Ok(None);
    }
    let forbid = |code: &str, error: &str| (StatusCode::FORBIDDEN, Json(refused(&origin, &class, target, action, code, error))).into_response();
    let why = request_why(body);
    let mut grant = None;
    let evidence_row = match decision {
        Decision::Deny { code, reason } => return Err(forbid(code, &reason)),
        Decision::Allow => None,
        Decision::NeedsGrant => {
            // The replay of an approved grant carries its code (server-only:
            // `worker_identity::enforce` drops any copy arriving on a socket).
            match headers.get(super::delegation_grants::GRANT_HEADER).and_then(|v| v.to_str().ok()) {
                Some(code) => match super::delegation_grants::claim_applying(state, code, &origin, &action.label(), &target.name()) {
                    Ok(ev) => {
                        grant = Some(code.to_string());
                        ev
                    }
                    Err(why) => return Err(forbid("grant_mismatch", &why)),
                },
                None => match super::delegation_grants::mode() {
                    // Passkey mode: a pending grant that needs the approval proof.
                    super::delegation_grants::Mode::Passkey => {
                        return Err(super::delegation_grants::request(state, &origin, &class.label(), &action.label(), &target.name(), why))
                    }
                    // Cite mode (default): a qualifying citation of Max's row
                    // applies the change now, recorded as a grant.
                    super::delegation_grants::Mode::Cite => {
                        let cite: Option<super::max_ref::Cite> = body.and_then(|b| b.get("cite")).filter(|c| !c.is_null())
                            .and_then(|c| serde_json::from_value(c.clone()).ok());
                        let terms = cite_terms(target, action);
                        let cited = match super::max_ref::cite_pair(state, cite.as_ref(), &origin, &terms) {
                            Ok(c) => c,
                            Err(r) => return Err(forbid(r.code(), &format!(
                                "{} needs a citation pair: cite {{ask_row_id, reply_row_id}}. The ask is your message into a session that names the target (one of {:?}) and the setting (one of {:?} for each); the reply is Max's own first typed message in that session after it, within 2 hours: {}",
                                action.label(), terms.target, terms.settings, r.code()))),
                        };
                        grant = Some(super::delegation_grants::begin_cited(state, &origin, &class.label(), &action.label(), &target.name(), why.clone(), &cited)?);
                        Some(cited.row_id)
                    }
                },
            }
        }
    };
    tracing::info!(actor = %origin, class = %class.label(), target = %target.name(), action = %action.label(),
        evidence_row = ?evidence_row, verdict = "delegation_allowed", measured = true, n_considered = 1,
        "a non-owner change passed the delegation table");
    // Authorization only (amux #41, ai-config #205 N6): the rename runs after
    // this, and `rename_session` logs its outcome (session_rename_done/failed).
    if let (Class::Lead { team, .. }, Action::RenameWorker { to }) = (&class, action) {
        tracing::info!(origin = %origin, target = %target.name(), to = %to, team = %team, verdict = "lead_team_rename_authorized",
            measured = true, n_considered = 1, "a lead was authorized to rename a worker in its own team");
    }
    Ok(Some(Grant { actor: origin, class, action: action.label(), target: target.name(), grant, evidence_row, why }))
}


/// The grant for an allowed non-owner change decided without a pending grant (a
/// create or apply that decides up front and logs after success).
pub(crate) fn grant_if_allowed(headers: &HeaderMap, target: &Target, action: &Action) -> Option<Grant> {
    let (origin, class, decision) = decide_for(headers, target, action);
    (class != Class::Owner && decision == Decision::Allow)
        .then(|| Grant { actor: origin, class, action: action.label(), target: target.name(), grant: None, evidence_row: None, why: None })
}

/// Whether `name` is a lead: CC_ORCHESTRATOR=1 and in `leads` (the one test;
/// the log reader uses it too).
pub(crate) fn is_lead(name: &str) -> bool {
    lead_teams(name).is_some()
}

/// Log an allowed non-owner change decided elsewhere (a create through
/// `worker_create_refusal`): re-derives the grant, commits it.
pub(crate) fn record_allowed(state: &AppState, headers: &HeaderMap, target: &Target, action: &Action, before: Value, after: Value) {
    let (origin, class, decision) = decide_for(headers, target, action);
    if class == Class::Owner || decision != Decision::Allow {
        return;
    }
    Grant { actor: origin, class, action: action.label(), target: target.name(), grant: None, evidence_row: None, why: None }
        .commit(state, before, after);
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

/// The action a config PATCH body asks for. EVERY key is classified (review
/// B1): `tags` or `rename` is its own action only when the body names nothing
/// else. Beside any other key the body is a `SetConfig` over all of its keys,
/// so `decide` judges each one and an unclassified key refuses it. Before
/// this, `{"tags":"dev","toggle_auto_continue":true}` was judged as a no-op
/// tag change and the handler then applied the YOLO toggle.
pub(crate) fn config_action(target: &str, body: &Value) -> Action {
    let keys: Vec<String> = body.as_object().map(|o| o.keys().filter(|k| !META_FIELDS.contains(&k.as_str())).cloned().collect()).unwrap_or_default();
    let only = |k: &str| keys.len() == 1 && keys[0] == k;
    if only("tags") {
        let to = super::session_verbs::tags_of(body.get("tags")).unwrap_or_else(|_| vec!["!invalid".into()]);
        let from: Vec<String> = super::session_verbs::lane_groups(target).into_iter().collect();
        return Action::SetTags { from, to };
    }
    if only("rename") {
        if let Some(to) = body.get("rename").and_then(Value::as_str) {
            return Action::RenameWorker { to: to.trim().to_string() };
        }
    }
    Action::SetConfig { keys }
}

/// H11: nobody but the owner gives a worker a reserved name, on any path.
pub(crate) fn reserved_name_refusal(headers: &HeaderMap, name: &str) -> Option<Response> {
    let origin = verified_origin(headers);
    if origin.is_empty() || !reserved_name(name) {
        return None;
    }
    let action = Action::RenameWorker { to: name.into() };
    Some((StatusCode::FORBIDDEN, Json(refused(&origin, &Class::Worker, &Target::Worker(name.into()), &action, "reserved_name",
        &format!("'{name}' is a reserved identity; only the owner may give a worker that name")))).into_response())
}

/// PATCH config, gated and logged: `PATCH /api/sessions/<w>/config` and
/// `PATCH /api/workers/{id}/config` both come here.
pub(crate) async fn guarded_config_patch(state: &AppState, headers: &HeaderMap, name: &str, body: &Value) -> Response {
    let action = config_action(name, body);
    if let Action::RenameWorker { to } = &action {
        if let Some(r) = reserved_name_refusal(headers, to) {
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

/// An untagged create by a lead gets the lead's team tag (its first team):
/// a lead's worker always lands inside its team, never untagged.
pub(crate) fn lead_default_tags(headers: &HeaderMap, tags: Vec<String>) -> Vec<String> {
    if !tags.is_empty() {
        return tags;
    }
    let origin = verified_origin(headers);
    if origin.is_empty() || origin == MRPOPO {
        return tags;
    }
    match lead_teams(&origin).and_then(|t| t.into_iter().next()) {
        Some(team) => {
            tracing::info!(actor = %origin, team = %team, verdict = "lead_create_auto_tagged", measured = true, n_considered = 1,
                "an untagged create by a lead was given the lead's team tag");
            vec![team]
        }
        None => tags,
    }
}

/// The settings a `POST /api/sessions` body writes besides name and tags:
/// every field with a set (truthy, non-empty) value, as `create:<field>`.
/// The chat type is the caller's `owner_only`; `start` is not a setting.
pub(crate) fn create_body_settings(body: &Value) -> Vec<String> {
    let set = |v: &Value| match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    };
    body.as_object().into_iter().flatten()
        .filter(|(k, v)| !matches!(k.as_str(), "name" | "tags" | "start" | "worker_type" | "cite" | "why") && set(v))
        .map(|(k, _)| format!("create:{k}"))
        .collect()
}

/// The settings a copy of `source` (duplicate/clone) writes: its env keys
/// other than identity/group bookkeeping, so a copy of a yolo worker is a
/// create with yolo.
pub(crate) fn copy_settings(source: &str) -> Vec<String> {
    super::session_verbs::parse_env(source).pairs().iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !matches!(*k, "CC_TAGS" | "CC_WORKER_TYPE" | "CC_CREATOR"))
        .map(|k| format!("env:{k}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead(t: &str) -> Class {
        Class::Lead { team: t.into(), teams: vec![t.into()] }
    }
    fn cfg(keys: &[&str]) -> Action {
        Action::SetConfig { keys: keys.iter().map(|k| k.to_string()).collect() }
    }
    fn tags(from: &[&str], to: &[&str]) -> Action {
        Action::SetTags { from: from.iter().map(|s| s.to_string()).collect(), to: to.iter().map(|s| s.to_string()).collect() }
    }
    fn create(tags: &[&str], settings: &[&str]) -> Action {
        Action::CreateWorker { tags: tags.iter().map(|s| s.to_string()).collect(), settings: settings.iter().map(|s| s.to_string()).collect(), owner_only: None }
    }
    const TEAM: TargetFacts = TargetFacts { in_team: true, privileged: false, is_actor: false, covers_actor: false };
    const PEER_LEAD: TargetFacts = TargetFacts { in_team: true, privileged: true, is_actor: false, covers_actor: false };
    const OTHER: TargetFacts = TargetFacts { in_team: false, privileged: false, is_actor: false, covers_actor: false };
    const ME: TargetFacts = TargetFacts { in_team: false, privileged: false, is_actor: true, covers_actor: true };
    /// A group or global layer the actor reads.
    const MY_LAYER: TargetFacts = TargetFacts { in_team: false, privileged: false, is_actor: false, covers_actor: true };
    fn code(d: Decision) -> String {
        match d {
            Decision::Deny { code, .. } => code.into(),
            Decision::Allow => "allow".into(),
            Decision::NeedsGrant => "grant".into(),
        }
    }

    #[test]
    fn delegation_owner_may_do_anything() {
        for a in [cfg(&["toggle_yolo"]), Action::SetSecret { key: "GITHUB_TOKEN".into() }, tags(&[], &["leads"]), Action::SetRouting,
            Action::CreateGroup { group: "x".into() }, cfg(&["env:AMUX_CONTRACT_DONE"]), Action::RenameWorker { to: "mrpopo".into() }] {
            assert_eq!(decide(&Class::Owner, &a, &OTHER), Decision::Allow, "{a:?}");
        }
    }

    #[test]
    fn delegation_lead_same_team_allowed_other_team_refused() {
        let l = lead("dev");
        assert_eq!(code(decide(&l, &create(&["dev"], &["create:dir", "create:desc", "create:model"]), &OTHER)), "allow");
        for (t, why) in [(&["leads"][..], "leads"), (&["ops"][..], "other team"), (&[][..], "untagged")] {
            assert_eq!(code(decide(&l, &create(t, &[]), &OTHER)), "worker_create_tags_outside_team", "{why}");
        }
        let chat = Action::CreateWorker { tags: vec!["dev".into()], settings: vec![], owner_only: Some("the chat worker type") };
        assert_eq!(code(decide(&l, &chat, &OTHER)), "worker_create_tags_outside_team");
        for a in [Action::StopWorker, Action::Lifecycle { verb: "start".into() }, cfg(&["model"]), cfg(&["effort"])] {
            assert_eq!(code(decide(&l, &a, &TEAM)), "allow", "{a:?}");
            assert_eq!(code(decide(&l, &a, &OTHER)), "peer_config_owner_only", "other team {a:?}");
            assert_eq!(code(decide(&l, &a, &PEER_LEAD)), "peer_config_owner_only", "peer lead {a:?}");
        }
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "dev".into() }, &OTHER)), "allow");
        assert_eq!(code(decide(&l, &Action::SetGroupConfig { group: "dev".into() }, &OTHER)), "allow");
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "ops".into() }, &OTHER)), "delegation_not_allowed");
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "leads".into() }, &OTHER)), "delegation_not_allowed");
    }

    #[test]
    fn delegation_lead_in_two_teams_creates_with_any_subset() {
        let l = Class::Lead { team: "dev".into(), teams: vec!["dev".into(), "ops".into()] };
        assert_eq!(code(decide(&l, &create(&["dev", "ops"], &[]), &OTHER)), "allow");
        assert_eq!(code(decide(&l, &create(&["ops"], &[]), &OTHER)), "allow");
        assert_eq!(code(decide(&l, &create(&["ops", "infra"], &[]), &OTHER)), "worker_create_tags_outside_team");
    }

    #[test]
    fn delegation_lead_create_classifies_every_setting() {
        let l = lead("dev");
        for k in ["create:yolo", "create:flags", "create:isolated", "create:creator", "env:CC_FLAGS", "env:CC_ISOLATED", "row:permissions", "row:environment", "env:CC_VERIFY"] {
            assert_eq!(code(decide(&l, &create(&["dev"], &[k]), &OTHER)), "worker_create_setting_refused", "{k}");
        }
    }

    #[test]
    fn delegation_lead_renames_non_lead_team_workers_only() {
        let rename = |to: &str| Action::RenameWorker { to: to.into() };
        assert_eq!(code(decide(&lead("dev"), &rename("dev-b"), &TEAM)), "allow");
        assert_eq!(code(decide(&lead("dev"), &rename("dev-b"), &PEER_LEAD)), "peer_config_owner_only");
        assert_eq!(code(decide(&lead("dev"), &rename("dev-b"), &OTHER)), "peer_config_owner_only");
        assert_eq!(code(decide(&lead("dev"), &rename("mrpopo"), &TEAM)), "reserved_name");
        // amux #41: a lead-looking name is the owner's to give.
        for to in ["dev-b-lead", "Dev-B-LEAD"] {
            assert_eq!(code(decide(&lead("dev"), &rename(to), &TEAM)), "lead_rename_reserved_suffix", "{to}");
        }
        assert_eq!(code(decide(&lead("dev"), &rename("dev-leader"), &TEAM)), "allow");
        assert_eq!(code(decide(&Class::Owner, &rename("dev-b-lead"), &TEAM)), "allow");
    }

    #[test]
    fn delegation_lead_may_not_change_a_team_workers_non_model_field() {
        let l = lead("dev");
        assert_eq!(code(decide(&l, &cfg(&["env:CC_MODEL"]), &TEAM)), "allow", "a lead sets CC_MODEL on its team");
        for k in ["desc", "dir", "toggle_yolo", "external_email_allowed", "isolated", "worktree_verify", "mcp", "keys"] {
            assert_eq!(code(decide(&l, &cfg(&[k]), &TEAM)), "peer_config_owner_only", "{k}");
        }
    }

    #[test]
    fn delegation_lead_tags_only_within_its_team() {
        let l = lead("dev");
        assert_eq!(code(decide(&l, &tags(&["dev"], &[]), &TEAM)), "allow", "removing T narrows");
        assert_eq!(code(decide(&l, &tags(&["dev"], &["dev"]), &TEAM)), "allow");
        for (from, to, f) in [(&["dev"][..], &["dev", "leads"][..], TEAM), (&["dev"][..], &["dev", "ops"][..], TEAM),
            (&["ops"][..], &["ops", "dev"][..], OTHER), (&["dev", "infra"][..], &["dev"][..], TEAM),
            (&["dev", "leads"][..], &["dev"][..], PEER_LEAD), (&["dev"][..], &[][..], PEER_LEAD)] {
            assert_eq!(code(decide(&l, &tags(from, to), &f)), "worker_group_membership_refused", "{from:?} -> {to:?}");
        }
    }

    #[test]
    fn delegation_mrpopo_needs_a_grant_per_row() {
        let m = Class::Mrpopo;
        assert_eq!(code(decide(&m, &cfg(&["env:CC_ORCHESTRATOR"]), &OTHER)), "grant");
        assert_eq!(code(decide(&m, &cfg(&["toggle_yolo", "external_email_allowed"]), &OTHER)), "grant", "both groups, one row");
        assert_eq!(code(decide(&m, &tags(&["dev"], &["dev", "leads"]), &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::MakeLead, &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::SetGroupRules { group: "leads".into() }, &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::SetGroupRules { group: "dev".into() }, &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::SetRouting, &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::SetGroupConfig { group: "dev".into() }, &OTHER)), "grant");
        assert_eq!(code(decide(&m, &Action::CreateGroup { group: "ops2".into() }, &OTHER)), "grant");
        // Other teams' membership: each changed group must be named.
        assert_eq!(code(decide(&m, &tags(&["dev"], &["dev", "ops"]), &OTHER)), "grant");
        // An owner-only key with no explicit row stays the owner's, on every layer.
        for k in ["env:CC_SOMETHING_OWNER_ONLY", "group:env:CC_SOMETHING_OWNER_ONLY", "global:env:CC_SOMETHING_OWNER_ONLY", "global:env:CC_MODEL"] {
            assert_eq!(code(decide(&m, &cfg(&[k]), &OTHER)), "delegation_not_allowed", "{k}");
        }
        // An explicit row keeps its phrase on the group and global layers.
        for k in ["group:env:AMUX_EMAIL_EXTERNAL_ALLOW", "global:env:AMUX_EMAIL_EXTERNAL_ALLOW"] {
            assert_eq!(code(decide(&m, &cfg(&[k]), &OTHER)), "grant", "{k}");
        }
        for (k, _first) in [("worktree_verify", "verify command"), ("env:CC_VERIFY", "verify command"), ("env:CC_DEPLOY_CHECK", "deploy check"),
            ("env:CC_WORKTREE_AUTO_MERGE", "auto-merge"), ("env:AMUX_YOLO_AUTO_YES", "auto-yes"), ("env:CC_PROJECT_LEAD", "project lead"),
            ("env:CC_RECEIVE_DENY", "receive deny")] {
            assert_eq!(code(decide(&m, &cfg(&[k]), &OTHER)), format!("grant"), "{k}");
        }
        for (k, _first) in [("toggle_yolo", "yolo"), ("external_email_allowed", "external email"), ("isolated", "isolated"),
            ("toggle_auto_continue", "auto-continue"), ("env:AMUX_NEEDS_INPUT_AUTO_PROD_DATA", "auto-approve"),
            ("env:AMUX_NEEDS_INPUT_AUTO_OUTBOUND", "auto-approve"), ("env:AMUX_BROWSER_PROFILES_ALLOW", "browser profile"),
            ("env:AMUX_APPROVAL_TYPES", "approval type"), ("scope:gates", "gates config"), ("scope:connectors", "connectors"),
            ("row:permissions", "permissions"), ("settings:commit-guard", "commit guard"), ("settings:task-guard", "task guard"),
            ("settings:default-model", "default model"), ("instance:apply", "config apply"), ("send_allow", "routing")] {
            assert_eq!(code(decide(&m, &cfg(&[k]), &OTHER)), format!("grant"), "{k}");
        }
        assert_eq!(code(decide(&m, &cfg(&["model"]), &OTHER)), "delegation_not_allowed");
        assert_eq!(code(decide(&m, &cfg(&["toggle_yolo", "desc"]), &OTHER)), "delegation_not_allowed");
        assert_eq!(code(decide(&m, &create(&[], &[]), &OTHER)), "worker_create_requires_lead");
        assert_eq!(code(decide(&m, &Action::StopWorker, &OTHER)), "delegation_not_allowed");
    }

    #[test]
    fn delegation_held_for_max_is_refused_even_for_mrpopo() {
        for c in [Class::Mrpopo, lead("dev"), Class::SelfWorker, Class::Worker] {
            for k in ["env:AMUX_CONTRACT_DONE", "env:AMUX_PANE_PROMPT_HISTORY", "env:AMUX_NEEDS_INPUT_AUTO_MONEY", "env:AMUX_NEEDS_INPUT_AUTO_MONEY_CAP", "env:AMUX_CARD_MAX_USD",
                "env:AMUX_DELEGATION_APPROVAL", "global:env:AMUX_DELEGATION_APPROVAL"] {
                assert_eq!(code(decide(&c, &cfg(&[k]), &TEAM)), "held_for_max", "{c:?} {k}");
            }
        }
        assert_eq!(code(decide(&Class::Mrpopo, &cfg(&["toggle_yolo"]), &ME)), "held_for_max", "mrpopo's own config");
        assert_eq!(code(decide(&Class::SelfWorker, &Action::RenameWorker { to: "mrpopo".into() }, &ME)), "reserved_name");
        assert_eq!(code(decide(&Class::SelfWorker, &Action::RenameWorker { to: " Owner ".into() }, &ME)), "reserved_name");
        assert!(reserved_name("MRPOPO") && !reserved_name("mr-popo"));
    }

    #[test]
    fn delegation_make_lead_is_mrpopo_through_a_grant_only() {
        let k = cfg(&["env:CC_ORCHESTRATOR"]);
        for c in [lead("dev"), Class::SelfWorker, Class::Worker] {
            assert_eq!(code(decide(&c, &Action::MakeLead, &TEAM)), "worker_group_membership_refused", "{c:?}");
        }
        assert_eq!(code(decide(&lead("dev"), &k, &TEAM)), "peer_config_owner_only");
        assert_eq!(code(decide(&Class::SelfWorker, &k, &ME)), "owner_only_key");
        assert_eq!(code(decide(&Class::Worker, &k, &OTHER)), "peer_config_owner_only");
        assert_eq!(code(decide(&lead("dev"), &tags(&["dev"], &["dev", "leads"]), &TEAM)), "worker_group_membership_refused");
    }

    #[test]
    fn delegation_self_non_privileged_only() {
        let s = Class::SelfWorker;
        for k in ["model", "effort", "desc", "task_summary", "env:CC_MODEL", "env:CC_DESC", "scope:memory", "keys"] {
            assert_eq!(code(decide(&s, &cfg(&[k]), &ME)), "allow", "{k}");
        }
        // ai-config #160's gap, and H4's default-allowed env keys.
        for k in ["external_email_allowed", "toggle_yolo", "toggle_auto_continue", "isolated", "worktree_verify",
            "env:CC_FLAGS", "env:AMUX_EMAIL_EXTERNAL_ALLOW", "env:AMUX_NEEDS_INPUT_AUTO_OUTBOUND", "env:CC_ORCHESTRATOR",
            "env:CC_VERIFY", "env:CC_DEPLOY_CHECK", "env:CC_WORKTREE_AUTO_MERGE", "env:AMUX_YOLO_AUTO_YES", "env:CC_PROJECT_LEAD",
            "env:CC_COMPANION_OF", "env:CC_RECEIVE_DENY", "send_allow", "scope:gates", "scope:rules", "row:environment", "an_unclassified_field"] {
            assert_eq!(code(decide(&s, &cfg(&[k]), &ME)), "owner_only_key", "{k}");
        }
        assert_eq!(code(decide(&s, &cfg(&["worker_type"]), &ME)), "worker_group_membership_refused");
        assert_eq!(code(decide(&s, &tags(&["dev"], &["dev", "leads"]), &ME)), "worker_group_membership_refused");
        assert_eq!(code(decide(&s, &cfg(&[]), &ME)), "delegation_not_allowed", "N8: no vacuous allow");
    }

    #[test]
    fn delegation_other_worker_refused_everything() {
        let w = Class::Worker;
        for a in [cfg(&["model"]), Action::StopWorker, Action::Lifecycle { verb: "keys".into() }, Action::RenameWorker { to: "x".into() }] {
            assert_eq!(code(decide(&w, &a, &OTHER)), "peer_config_owner_only", "{a:?}");
        }
        assert_eq!(code(decide(&w, &create(&[], &[]), &OTHER)), "worker_create_requires_lead");
        assert_eq!(code(decide(&w, &tags(&[], &["dev"]), &OTHER)), "worker_group_membership_refused");
        assert_eq!(code(decide(&w, &Action::SetGroupRules { group: "dev".into() }, &OTHER)), "delegation_not_allowed");
        assert_eq!(code(decide(&w, &Action::SetRouting, &OTHER)), "delegation_not_allowed");
    }

    #[test]
    fn delegation_secrets_are_never_delegable() {
        for c in [lead("dev"), Class::Mrpopo, Class::SelfWorker, Class::Worker] {
            for a in [Action::SetSecret { key: "X".into() }, cfg(&["env:GITHUB_TOKEN"]), cfg(&["env:AMUX_WORKER_TOKEN"]),
                cfg(&["env:OPENAI_API_KEY"]), cfg(&["model", "env:DB_PASSWORD"]), cfg(&["group:env:SLACK_SECRET"]), create(&["dev"], &["env:API_TOKEN"])] {
                assert_eq!(code(decide(&c, &a, &TEAM)), "secret_owner_only", "{c:?} {a:?}");
            }
        }
    }

    #[test]
    fn delegation_every_config_patch_field_is_classified() {
        for k in super::super::session_verbs::config_patch_fields_for_delegation() {
            assert!(FIELDS.iter().any(|(f, _)| *f == k) || GRANT_ROWS.contains(&k),
                "config PATCH field '{k}' has no delegation row");
        }
    }

    /// Review B1's sweep, by construction: every key the config PATCH handler
    /// reads (each `.get("<key>")` in its body and each board-automation
    /// field) is in the one-setting guard and has a delegation class. Reads
    /// the shipped handler's source, so a new branch without a row fails.
    #[test]
    fn config_patch_ops_cover_every_key_the_handler_reads() {
        let src = include_str!("session_verbs.rs");
        let start = src.find("async fn config_patch_with_liveness(").expect("handler");
        let body = &src[start..start + src[start..].find("\n}\n").expect("handler end")];
        let get = regex::Regex::new(r#"\.get\(\s*"([a-z][a-z_]*)"\s*\)"#).unwrap();
        let mut read: BTreeSet<String> = get.captures_iter(body).map(|c| c[1].to_string()).collect();
        let auto = &body[body.find("let automation = [").expect("automation table")..];
        let auto = &auto[..auto.find("];").unwrap()];
        let field = regex::Regex::new(r#"\(\s*"([a-z][a-z_]*)",\s"#).unwrap();
        let fields: Vec<String> = field.captures_iter(auto).map(|c| c[1].to_string()).collect();
        assert_eq!(fields.len(), 6, "the six board-automation fields: {fields:?}");
        read.extend(fields);
        // Read inside a branch from something other than the body.
        read.remove("expires_hours");
        assert!(read.len() >= 30, "the scan found the handler's keys: {read:?}");
        let ops: BTreeSet<String> = super::super::session_verbs::config_patch_fields_for_delegation().into_iter().map(String::from).collect();
        let missing: Vec<&String> = read.iter().filter(|k| !ops.contains(*k)).collect();
        assert!(missing.is_empty(), "keys the handler reads with no one-setting row: {missing:?}");
        for k in &read {
            assert!(FIELDS.iter().any(|(f, _)| f == k) || GRANT_ROWS.contains(&k.as_str()), "'{k}' has no delegation class");
        }
    }

    /// Review B4: every grant row has plain words an ask can name it by.
    #[test]
    fn every_grant_row_has_plain_words() {
        for row in GRANT_ROWS {
            assert!(SETTING_WORDS.iter().any(|(r, w)| r == row && !w.is_empty()), "grant row '{row}' has no plain words");
        }
        let t = cite_terms(&Target::Group("leads".into()), &cfg(&["group:env:CC_FLAGS"]));
        assert_eq!(t.target, vec!["leads".to_string()]);
        assert!(t.settings[0].contains(&"yolo".to_string()) && t.settings[0].contains(&"CC_FLAGS".to_string()));
        let t = cite_terms(&Target::Worker("review-dev-a".into()), &tags(&["dev"], &["dev", "ops"]));
        assert_eq!(t.settings, vec![vec!["ops".to_string()]], "a tag change names each changed group");
    }

    /// Review B1: `tags` (or `rename`) beside any other key is no longer a tag
    /// change; every key is judged, so the bundled key cannot ride on it.
    #[test]
    fn delegation_a_mixed_config_body_is_judged_key_by_key() {
        let board = ["auto_drain_backlog", "board_auto_pickup", "board_auto_continue", "board_standing_orders", "board_decompose", "board_force_adherence"];
        let mut extras: Vec<&str> = vec!["toggle_auto_continue", "toggle_yolo", "external_email_allowed", "an_unclassified_key"];
        extras.extend(board);
        for extra in extras {
            for lead_key in ["tags", "rename"] {
                let body = json!({lead_key: "dev", extra: true});
                let a = config_action("review-nobody", &body);
                let Action::SetConfig { keys } = &a else { panic!("{body}: judged as {a:?}, not key by key") };
                assert_eq!(keys.iter().map(String::as_str).collect::<BTreeSet<_>>(), [lead_key, extra].into_iter().collect(), "{body}");
                assert_ne!(code(decide(&lead("dev"), &a, &TEAM)), "allow", "lead: {body}");
                assert!(matches!(code(decide(&Class::Mrpopo, &a, &OTHER)).as_str(), "worker_group_membership_refused" | "delegation_not_allowed"), "mrpopo: {body}");
            }
        }
        // Each board field alone is the owner's: a lead is refused on its team.
        for k in board {
            assert_eq!(code(decide(&lead("dev"), &config_action("review-nobody", &json!({k: true})), &TEAM)), "peer_config_owner_only", "{k}");
            assert_eq!(code(decide(&Class::Mrpopo, &cfg(&[k]), &OTHER)), "delegation_not_allowed", "{k}");
        }
        // A lone tags/rename (with metadata) keeps its own row.
        assert!(matches!(config_action("review-nobody", &json!({"tags": "dev", "why": "x", "cite": null})), Action::SetTags { .. }));
        assert!(matches!(config_action("review-nobody", &json!({"rename": "dev-b"})), Action::RenameWorker { .. }));
    }

    /// Review B3: a write to a layer that covers the actor (its own group, the
    /// global layer, an instance-wide setting) is a write to the actor itself.
    #[test]
    fn delegation_a_layer_that_covers_the_actor_is_held() {
        let m = Class::Mrpopo;
        for k in ["group:env:CC_FLAGS", "global:env:CC_SEND_ALLOW", "global:env:AMUX_EMAIL_EXTERNAL_ALLOW", "settings:default-model", "group:scope:connectors"] {
            assert_eq!(code(decide(&m, &cfg(&[k]), &MY_LAYER)), "held_for_max", "mrpopo, own layer: {k}");
        }
        assert_eq!(code(decide(&m, &cfg(&["group:env:CC_FLAGS"]), &OTHER)), "grant", "a layer without mrpopo stays a grant row");
        assert_eq!(code(decide(&m, &Action::SetRouting, &MY_LAYER)), "held_for_max");
        // Non-privileged on its own layer: the table still decides.
        assert_eq!(code(decide(&m, &Action::SetGroupRules { group: "leads".into() }, &MY_LAYER)), "grant");
        assert_eq!(code(decide(&m, &Action::SetGroupConfig { group: "leads".into() }, &MY_LAYER)), "grant");
        // A lead on a layer it reads.
        let l = lead("dev");
        assert_eq!(code(decide(&l, &cfg(&["group:env:CC_FLAGS"]), &MY_LAYER)), "held_for_max");
        assert_eq!(code(decide(&l, &cfg(&["global:env:CC_SEND_ALLOW"]), &MY_LAYER)), "held_for_max");
        assert_eq!(code(decide(&l, &Action::SetGroupRules { group: "dev".into() }, &MY_LAYER)), "allow", "its team's rules are not authority");
    }

    #[test]
    fn delegation_facts_say_which_layers_cover_the_actor() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(dir.path());
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        std::fs::write(dir.path().join("sessions/mrpopo.env"), "CC_TAGS=\"leads\"\n").unwrap();
        let a = cfg(&["group:env:CC_FLAGS"]);
        let f = |t: Target| facts(MRPOPO, &Class::Mrpopo, &t, &a).covers_actor;
        assert!(f(Target::Group("leads".into())), "its own group");
        assert!(f(Target::Group(" Leads ".into())), "normalized");
        assert!(!f(Target::Group("dev".into())), "a group it is not in");
        assert!(f(Target::Fleet) && f(Target::Settings(vec![])), "global and instance-wide");
        assert!(f(Target::Worker(MRPOPO.into())) && !f(Target::Worker("review-dev-a".into())));
    }

    /// An unproven claim acting on ITSELF is not configuring another worker
    /// (dev-lead, #18): it reaches decide_self's non-privileged set, as on
    /// main. A privileged key on itself stays the owner's (ai-config #160),
    /// and on any other target the claim stays unverified.
    #[test]
    fn an_unproven_self_claim_reaches_only_the_non_privileged_self_set() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(dir.path());
        let mut h = HeaderMap::new();
        h.insert("x-amux-session", "lane-x".parse().unwrap());
        let me = Target::Worker("lane-x".into());
        let (origin, class, d) = decide_for(&h, &me, &Action::Lifecycle { verb: "pause".into() });
        assert_eq!((origin.as_str(), &class, code(d).as_str()), ("lane-x", &Class::SelfWorker, "allow"));
        assert_eq!(code(decide_for(&h, &me, &cfg(&["scope:memory"])).2), "allow");
        for k in ["scope:connectors", "scope:gates"] {
            assert_eq!(code(decide_for(&h, &me, &cfg(&[k])).2), "owner_only_key", "{k} on itself stays the owner's");
        }
        let (origin, class, d) = decide_for(&h, &Target::Worker("lane-y".into()), &cfg(&["scope:memory"]));
        assert_eq!(origin, super::super::worker_identity::UNVERIFIED_IDENTITY);
        assert_eq!((class, code(d).as_str()), (Class::Worker, "peer_config_owner_only"));
    }

    /// The unproven self case is forced to SelfWorker, never classified: a
    /// claim to be mrpopo acting on mrpopo gets no Mrpopo powers.
    #[test]
    fn an_unproven_mrpopo_self_claim_gets_no_mrpopo_powers() {
        let dir = tempfile::tempdir().unwrap();
        let _home = crate::api::settings::test_env::set_home(dir.path());
        let mut h = HeaderMap::new();
        h.insert("x-amux-session", MRPOPO.parse().unwrap());
        let (_, class, d) = decide_for(&h, &Target::Worker(MRPOPO.into()), &cfg(&["scope:connectors"]));
        assert_eq!(class, Class::SelfWorker);
        assert_eq!(code(d), "owner_only_key");
    }
}
