//! Connection-based message attribution. No environment header proves a pane.
//! Kept dependency-free so the shipped decision can be tested without a server.

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Worker(String),
    Owner,
    Claim { pane: Option<String>, claim: String },
    Unknown,
}

pub(crate) struct Evidence<'a> {
    pub pane: &'a str,
    pub origin: &'a str,
    pub claim: &'a str,
    pub owner_ui: bool,
    pub member: bool,
    pub body_claim: bool,
}

/// The connections that may carry owner input, and only with the UI token
/// (#10, B1): a code-signed GUI browser outside every pane and launchd job, or
/// a remote tailnet peer. A local CLI or script holding the token, the owner's
/// own tmux pane, and a non-tailnet network peer are not owner connections.
pub(crate) fn owner_connection(origin: &str) -> bool {
    matches!(origin, "browser" | "remote")
}

pub(crate) fn decide(e: Evidence<'_>) -> Decision {
    if e.pane.is_empty() && !matches!(e.origin, "outside" | "remote" | "owner-pane" | "browser" | "lan") {
        return Decision::Unknown;
    }
    let mismatch = e.claim.split(" / ").any(|c| !c.is_empty() && c != e.pane);
    if mismatch {
        return Decision::Claim {
            pane: (!e.pane.is_empty()).then(|| e.pane.to_string()),
            claim: e.claim.into(),
        };
    }
    if !e.pane.is_empty() {
        return Decision::Worker(e.pane.into());
    }
    if owner_connection(e.origin) && e.owner_ui && !e.member && !e.body_claim {
        return Decision::Owner;
    }
    Decision::Unknown
}

/// Only positively proved owner input or server automation may use the bare
/// command allow-list. Worker self-send is still worker input.
pub(crate) fn may_type_bare(owner: bool, server: bool) -> bool {
    owner || server
}

/// Callers sanitize both fields before rendering them in a single stamp line.
pub(crate) fn claim_label(pane: Option<&str>, claim: &str) -> String {
    let source = pane
        .map(|p| format!("process in worker {p}"))
        .unwrap_or_else(|| "outside process".into());
    format!("{source}, claims {claim}; unverified;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence<'a>(pane: &'a str, origin: &'a str, claim: &'a str) -> Evidence<'a> {
        Evidence {
            pane,
            origin,
            claim,
            owner_ui: false,
            member: false,
            body_claim: false,
        }
    }

    #[test]
    fn outside_process_claim_never_becomes_owner_or_bare() {
        let e = Evidence {
            owner_ui: true,
            ..evidence("", "outside", "dev-lead")
        };
        let d = decide(e);
        assert_eq!(
            d,
            Decision::Claim {
                pane: None,
                claim: "dev-lead".into()
            }
        );
        assert_eq!(
            claim_label(None, "dev-lead"),
            "outside process, claims dev-lead; unverified;"
        );
        assert!(!may_type_bare(matches!(d, Decision::Owner), false));
    }

    #[test]
    fn pane_a_claiming_b_names_both_without_verifying_b() {
        assert_eq!(
            decide(evidence("A", "worker", "B")),
            Decision::Claim {
                pane: Some("A".into()),
                claim: "B".into()
            }
        );
        assert_eq!(
            claim_label(Some("A"), "B"),
            "process in worker A, claims B; unverified;"
        );
    }

    #[test]
    fn own_pane_verifies_a_even_without_environment_headers() {
        for claim in ["A", "A / A", ""] {
            assert_eq!(
                decide(evidence("A", "worker", claim)),
                Decision::Worker("A".into())
            );
        }
        assert!(!may_type_bare(false, false));
    }

    #[test]
    fn owner_requires_a_browser_or_tailnet_connection_and_the_ui_token() {
        for origin in ["browser", "remote"] {
            assert_eq!(
                decide(Evidence {
                    owner_ui: true,
                    ..evidence("", origin, "")
                }),
                Decision::Owner,
                "{origin} with the UI token"
            );
            assert_eq!(decide(evidence("", origin, "")), Decision::Unknown, "{origin} without the UI token");
        }
        assert!(may_type_bare(true, false));
        assert!(matches!(
            decide(Evidence {
                owner_ui: true,
                ..evidence("", "browser", "B")
            }),
            Decision::Claim { .. }
        ));
        assert_eq!(
            decide(Evidence {
                owner_ui: true,
                ..evidence("", "unknown", "")
            }),
            Decision::Unknown
        );
        assert_eq!(
            decide(Evidence {
                owner_ui: true,
                ..evidence("A", "worker", "")
            }),
            Decision::Worker("A".into())
        );
        for e in [
            Evidence {
                member: true,
                ..evidence("", "browser", "")
            },
            Evidence {
                body_claim: true,
                ..evidence("", "remote", "")
            },
        ] {
            assert_eq!(
                decide(Evidence {
                    owner_ui: true,
                    ..e
                }),
                Decision::Unknown
            );
        }
    }

    /// B1: the UI token is derivable by any same-user process, so holding it
    /// over a local CLI connection (a curl, a script, the owner's own tmux
    /// pane) or from a non-tailnet network peer never makes the owner.
    #[test]
    fn a_token_bearing_local_or_lan_process_is_not_the_owner() {
        for origin in ["outside", "owner-pane", "lan"] {
            assert_eq!(
                decide(Evidence {
                    owner_ui: true,
                    ..evidence("", origin, "")
                }),
                Decision::Unknown,
                "{origin} holding the UI token"
            );
            assert_eq!(decide(evidence("", origin, "")), Decision::Unknown, "{origin}");
        }
        assert!(!owner_connection("outside") && !owner_connection("owner-pane") && !owner_connection("lan"));
    }
}
