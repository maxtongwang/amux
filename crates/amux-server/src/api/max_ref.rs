//! STUB for piece (b) of maxtongwang/ai-config#160 (feat/delegation-max-ref).
//! Exact interface signatures only, so piece (a) compiles before (b) lands.
//! It refuses every reference. It is deleted when (b)'s branch is merged.
#![allow(dead_code)] // stub: items are used once the real module is merged
use super::AppState;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq)]
pub(crate) struct MaxRef {
    pub session: String,
    pub time: f64,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub(crate) struct MaxRefEvidence {
    pub row_id: i64,
    pub session: String,
    pub ts: f64,
    pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MaxRefRefusal {
    Missing,
    NoSuchRow,
    NotHumanTyped,
    WrongSession,
    Stale,
    OutsideWindow,
    DoesNotNameTarget,
}

impl MaxRefRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Missing => "max_ref_missing",
            Self::NoSuchRow => "max_ref_no_such_row",
            Self::NotHumanTyped => "max_ref_not_human_typed",
            Self::WrongSession => "max_ref_wrong_session",
            Self::Stale => "max_ref_stale",
            Self::OutsideWindow => "max_ref_outside_window",
            Self::DoesNotNameTarget => "max_ref_does_not_name_target",
        }
    }

    pub(crate) fn message(&self) -> String {
        format!("max_ref refused ({})", self.code())
    }
}

pub(crate) const MAX_REF_WINDOW_S: f64 = 600.0;
pub(crate) const MAX_REF_MAX_AGE_S: f64 = 86_400.0;

/// One human-typed row naming a target AND one of `terms` (stub: refuses).
pub(crate) fn verify_max_ref_terms(
    state: &AppState,
    r: Option<&MaxRef>,
    targets: &[&str],
    _terms: &[&str],
    now: f64,
) -> Result<MaxRefEvidence, MaxRefRefusal> {
    verify_max_ref(state, r, targets, now)
}

pub(crate) fn verify_max_ref(
    _state: &AppState,
    r: Option<&MaxRef>,
    _targets: &[&str],
    _now: f64,
) -> Result<MaxRefEvidence, MaxRefRefusal> {
    match r {
        None => Err(MaxRefRefusal::Missing),
        Some(_) => Err(MaxRefRefusal::NoSuchRow),
    }
}
