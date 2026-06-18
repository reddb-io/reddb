//! `PolicyLinter` — pure-function linter for IAM policy documents.
//!
//! Issue #710. Builds on the [`ActionCatalog`](crate::auth::action_catalog)
//! (S1A, #707) to produce structured diagnostics about a policy *without*
//! rejecting it. Unlike [`Policy::from_json_str`], which validates
//! eagerly and refuses to parse a policy that references an unknown
//! action, the linter walks the raw JSON loosely and emits one
//! diagnostic per finding so operator tooling can present a complete
//! report.
//!
//! This slice ships four diagnostic kinds:
//!
//! * [`DiagnosticCode::UnknownAction`] — action verb absent from the
//!   catalog (severity: error).
//! * [`DiagnosticCode::DeprecatedAction`] — action is `Deprecated` in
//!   the catalog; the diagnostic carries the `replacement` hint from
//!   the catalog entry (severity: warning).
//! * [`DiagnosticCode::SuspectResource`] — resource is missing a
//!   `<kind>:` prefix, or is bare `*` (severity: warning).
//! * [`DiagnosticCode::NoEffectStatements`] — an `Allow` statement is
//!   strictly shadowed by a no-condition `Deny` with overlapping
//!   action/resource sets, making the Allow effectively dead code
//!   (severity: warning).
//! * [`DiagnosticCode::SelfLockRisk`] — the candidate policy, if
//!   attached, would fail the [`crate::auth::self_lock_guard`]
//!   invariant, i.e. would prevent the synthetic platform owner from
//!   ever detaching policies again. Severity is `error` because the
//!   attach-time guard will refuse the same policy.
//!
//! ## Diagnostic ordering
//!
//! Diagnostics are emitted in a deterministic order: by `(severity,
//! code, location)` after they have been collected, so two runs on the
//! same input always produce the same sequence. `Error` sorts before
//! `Warning` so the most actionable findings surface first.

use std::cmp::Ordering;

use crate::auth::action_catalog::{lookup, LifecycleState};
use crate::serde_json::{self, Value};

/// Severity of a [`Diagnostic`].
///
/// Ordering: `Error < Warning`. The linter sorts by severity ascending
/// so errors come first in the output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// Stable lowercase identifier used by the SQL + HTTP surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Severity::Error => 0,
            Severity::Warning => 1,
        }
    }
}

/// Stable diagnostic code. Operator tooling matches on the string form
/// (via [`DiagnosticCode::as_str`]), so these names are part of the
/// public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    UnknownAction,
    DeprecatedAction,
    SuspectResource,
    NoEffectStatements,
    SelfLockRisk,
}

impl DiagnosticCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiagnosticCode::UnknownAction => "unknown_action",
            DiagnosticCode::DeprecatedAction => "deprecated_action",
            DiagnosticCode::SuspectResource => "suspect_resource",
            DiagnosticCode::NoEffectStatements => "no_effect_statements",
            DiagnosticCode::SelfLockRisk => "self_lock_risk",
        }
    }
}

/// One structured finding produced by the linter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    /// Optional remediation hint — for `DeprecatedAction` this carries
    /// the catalog's `replacement` verb verbatim.
    pub suggested_fix: Option<String>,
    /// Dotted path inside the policy document (`statements[0].actions[1]`).
    pub location: Option<String>,
}

impl Diagnostic {
    /// JSON encoding shared by the SQL and HTTP surfaces.
    pub fn to_json_value(&self) -> Value {
        use crate::serde_json::Map;
        let mut obj = Map::new();
        obj.insert(
            "severity".into(),
            Value::String(self.severity.as_str().into()),
        );
        obj.insert("code".into(), Value::String(self.code.as_str().into()));
        obj.insert("message".into(), Value::String(self.message.clone()));
        obj.insert(
            "suggested_fix".into(),
            self.suggested_fix
                .as_ref()
                .map(|s| Value::String(s.clone()))
                .unwrap_or(Value::Null),
        );
        obj.insert(
            "location".into(),
            self.location
                .as_ref()
                .map(|s| Value::String(s.clone()))
                .unwrap_or(Value::Null),
        );
        Value::Object(obj)
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Lint a policy document supplied as raw JSON.
///
/// Always returns a `Vec<Diagnostic>` — a top-level parse failure is
/// itself encoded as a single `Error` diagnostic so callers don't have
/// to branch on `Result`. A clean policy returns an empty vec.
pub fn lint(policy_json: &str) -> Vec<Diagnostic> {
    let value: Value = match serde_json::from_str(policy_json) {
        Ok(v) => v,
        Err(msg) => {
            return vec![Diagnostic {
                severity: Severity::Error,
                code: DiagnosticCode::UnknownAction, // best-effort code reuse — see note
                message: format!("policy json failed to parse: {msg}"),
                suggested_fix: None,
                location: None,
            }];
        }
    };
    lint_value(&value)
}

/// Lint an already-parsed [`Value`]. The HTTP and SQL surfaces use
/// this entry point after they have decoded the body / fetched the
/// stored document so they don't have to re-stringify.
pub fn lint_value(policy: &Value) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();

    let Some(obj) = policy.as_object() else {
        out.push(Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode::UnknownAction,
            message: "policy json must be an object".into(),
            suggested_fix: None,
            location: None,
        });
        return finalize(out);
    };
    let Some(statements) = obj.get("statements").and_then(|s| s.as_array()) else {
        out.push(Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode::UnknownAction,
            message: "policy.statements must be an array".into(),
            suggested_fix: None,
            location: None,
        });
        return finalize(out);
    };

    // Pass 1: per-statement diagnostics (action verbs + resources).
    let mut parsed_statements: Vec<ParsedStatement> = Vec::with_capacity(statements.len());
    for (s_idx, st) in statements.iter().enumerate() {
        let parsed = lint_statement(s_idx, st, &mut out);
        parsed_statements.push(parsed);
    }

    // Pass 2: cross-statement NoEffectStatements check.
    lint_no_effect(&parsed_statements, &mut out);

    // Pass 3: SelfLockRisk — reuse the attach-time guard from S6 so
    // operators see the identical explanation at author time.
    lint_self_lock(policy, &parsed_statements, &mut out);

    finalize(out)
}

fn finalize(mut out: Vec<Diagnostic>) -> Vec<Diagnostic> {
    // Stable ordering: severity (Error < Warning), then code, then location.
    out.sort_by(|a, b| {
        a.severity
            .rank()
            .cmp(&b.severity.rank())
            .then_with(|| a.code.as_str().cmp(b.code.as_str()))
            .then_with(|| match (&a.location, &b.location) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
            .then_with(|| a.message.cmp(&b.message))
    });
    out
}

// ---------------------------------------------------------------------------
// Statement-level passes
// ---------------------------------------------------------------------------

/// Loose-parsed shape of a statement used by the cross-statement
/// no-effect check. We keep raw strings here rather than ActionPattern
/// / ResourcePattern because the linter must run even on documents
/// that reference unknown actions, and `compile_action` would silently
/// promote a typo to `Exact(...)` and hide the bug.
struct ParsedStatement {
    effect: Option<String>,
    actions: Vec<String>,
    resources: Vec<String>,
    has_condition: bool,
    sid: Option<String>,
}

fn lint_statement(s_idx: usize, st: &Value, out: &mut Vec<Diagnostic>) -> ParsedStatement {
    let mut parsed = ParsedStatement {
        effect: None,
        actions: Vec::new(),
        resources: Vec::new(),
        has_condition: false,
        sid: None,
    };
    let Some(obj) = st.as_object() else {
        return parsed;
    };

    parsed.effect = obj
        .get("effect")
        .and_then(|e| e.as_str())
        .map(|s| s.to_ascii_lowercase());
    parsed.has_condition = matches!(obj.get("condition"), Some(c) if !matches!(c, Value::Null));
    parsed.sid = obj.get("sid").and_then(|s| s.as_str()).map(|s| s.into());

    if let Some(actions) = obj.get("actions").and_then(|a| a.as_array()) {
        for (a_idx, a) in actions.iter().enumerate() {
            let Some(name) = a.as_str() else { continue };
            parsed.actions.push(name.to_string());
            check_action(s_idx, a_idx, name, out);
        }
    }
    if let Some(resources) = obj.get("resources").and_then(|r| r.as_array()) {
        for (r_idx, r) in resources.iter().enumerate() {
            let Some(name) = r.as_str() else { continue };
            parsed.resources.push(name.to_string());
            check_resource(s_idx, r_idx, name, out);
        }
    }
    parsed
}

fn check_action(s_idx: usize, a_idx: usize, name: &str, out: &mut Vec<Diagnostic>) {
    let location = format!("statements[{s_idx}].actions[{a_idx}]");
    match lookup(name) {
        None => {
            out.push(Diagnostic {
                severity: Severity::Error,
                code: DiagnosticCode::UnknownAction,
                message: format!("action `{name}` is not in the action catalog"),
                suggested_fix: None,
                location: Some(location),
            });
        }
        Some(entry) => {
            if let LifecycleState::Deprecated {
                replacement,
                since_version,
            } = &entry.lifecycle_state
            {
                let message = match replacement {
                    Some(r) => format!(
                        "action `{name}` was deprecated in {since_version}; use `{r}` instead",
                    ),
                    None => format!("action `{name}` was deprecated in {since_version}",),
                };
                out.push(Diagnostic {
                    severity: Severity::Warning,
                    code: DiagnosticCode::DeprecatedAction,
                    message,
                    suggested_fix: replacement.map(|s| s.to_string()),
                    location: Some(location),
                });
            }
        }
    }
}

fn check_resource(s_idx: usize, r_idx: usize, raw: &str, out: &mut Vec<Diagnostic>) {
    let location = format!("statements[{s_idx}].resources[{r_idx}]");
    // Bare `*` is the global wildcard — flagged because it is rarely
    // what an operator means. Use a kind-scoped `<kind>:*` instead.
    if raw == "*" {
        out.push(Diagnostic {
            severity: Severity::Warning,
            code: DiagnosticCode::SuspectResource,
            message: "resource `*` matches everything; scope with `<kind>:*` instead".into(),
            suggested_fix: Some("<kind>:*".into()),
            location: Some(location),
        });
        return;
    }
    // Missing `<kind>:` prefix — e.g. `"orders"` or `"public.orders"`
    // without a leading namespace.
    if !raw.contains(':') {
        out.push(Diagnostic {
            severity: Severity::Warning,
            code: DiagnosticCode::SuspectResource,
            message: format!(
                "resource `{raw}` is missing a `<kind>:` prefix (e.g. `table:{raw}`)",
            ),
            suggested_fix: Some(format!("<kind>:{raw}")),
            location: Some(location),
        });
    }
}

// ---------------------------------------------------------------------------
// Cross-statement: NoEffectStatements
// ---------------------------------------------------------------------------

/// Flag Allow statements that are universally shadowed by an
/// unconditional Deny on the same action/resource pair.
///
/// Heuristic: for each `(Allow, Deny)` pair, if
///
/// 1. the action sets share at least one entry,
/// 2. the resource sets share at least one entry, and
/// 3. the Deny statement has no `condition` (so it always fires when
///    its action/resource match),
///
/// then the Allow's intersected matches will always be overridden by
/// the Deny. We surface a single diagnostic against the Allow with the
/// shadowing Deny statement index in the message.
fn lint_no_effect(stmts: &[ParsedStatement], out: &mut Vec<Diagnostic>) {
    for (a_idx, a) in stmts.iter().enumerate() {
        if a.effect.as_deref() != Some("allow") {
            continue;
        }
        for (d_idx, d) in stmts.iter().enumerate() {
            if a_idx == d_idx {
                continue;
            }
            if d.effect.as_deref() != Some("deny") {
                continue;
            }
            if d.has_condition {
                // The Deny only fires when its condition holds — it
                // does not shadow the Allow universally. The brief's
                // "disjoint condition sets" wording maps to this:
                // when the Deny carries a condition, the Allow may
                // still apply in the complementary window.
                continue;
            }
            let action_overlap: Vec<&String> = a
                .actions
                .iter()
                .filter(|x| d.actions.iter().any(|y| y == *x))
                .collect();
            if action_overlap.is_empty() {
                continue;
            }
            let resource_overlap: Vec<&String> = a
                .resources
                .iter()
                .filter(|x| d.resources.iter().any(|y| y == *x))
                .collect();
            if resource_overlap.is_empty() {
                continue;
            }
            out.push(Diagnostic {
                severity: Severity::Warning,
                code: DiagnosticCode::NoEffectStatements,
                message: format!(
                    "Allow statement is shadowed by unconditional Deny at statements[{d_idx}] \
                     (overlapping actions: {actions:?}, resources: {resources:?})",
                    actions = action_overlap,
                    resources = resource_overlap,
                ),
                suggested_fix: Some(
                    "narrow the Deny with a condition, or remove the redundant Allow".into(),
                ),
                location: Some(format!("statements[{a_idx}]")),
            });
            // One diagnostic per Allow is enough — don't spam if
            // multiple Denys shadow the same Allow.
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// SelfLockRisk — author-time mirror of `PolicySelfLockGuard` (S6, #713)
// ---------------------------------------------------------------------------

/// Feed the candidate policy through the same simulation the attach-time
/// guard runs and, if the invariant would refuse the attach, surface a
/// diagnostic whose `message` is the verbatim attach-time error string.
///
/// If the policy fails to parse via [`Policy::from_json_str`] — typically
/// because an `UnknownAction` diagnostic has already flagged a typo — we
/// skip the check rather than double-reporting. The other diagnostic
/// kinds tell the operator what's wrong before they ever reach this
/// pass.
fn lint_self_lock(
    policy: &Value,
    parsed_statements: &[ParsedStatement],
    out: &mut Vec<Diagnostic>,
) {
    use std::sync::Arc;

    use crate::auth::policies::Policy;
    use crate::auth::self_lock_guard::{
        check_self_lock_invariant, format_block_error, InvariantOutcome,
    };

    let policy_json = policy.to_string_compact();
    let Ok(parsed) = Policy::from_json_str(&policy_json) else {
        return;
    };
    let outcome = check_self_lock_invariant(&[Arc::new(parsed)]);
    let InvariantOutcome::Blocked { ref sid, .. } = outcome else {
        return;
    };
    let Some(message) = format_block_error(&outcome) else {
        return;
    };

    // Best-effort location: if the guard named a sid, map it back to
    // a statement index in the raw document so operator tooling can
    // jump to the offending statement.
    let location = sid.as_ref().and_then(|target| {
        parsed_statements
            .iter()
            .position(|st| st.sid.as_deref() == Some(target.as_str()))
            .map(|idx| format!("statements[{idx}]"))
    });

    out.push(Diagnostic {
        severity: Severity::Error,
        code: DiagnosticCode::SelfLockRisk,
        message,
        suggested_fix: Some(
            "narrow the Deny with a condition (e.g. `platform_scoped: false`) or remove it".into(),
        ),
        location,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_policy() -> &'static str {
        r#"{
            "id": "p1",
            "version": 1,
            "statements": [
                {
                    "effect": "allow",
                    "actions": ["select"],
                    "resources": ["table:public.orders"]
                }
            ]
        }"#
    }

    #[test]
    fn clean_policy_produces_no_diagnostics() {
        assert!(lint(clean_policy()).is_empty());
    }

    #[test]
    fn unknown_action_is_flagged_as_error() {
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["definitely-not-an-action"],"resources":["table:foo"]}
        ]}"#;
        let diags = lint(p);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, DiagnosticCode::UnknownAction);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(
            diags[0].location.as_deref(),
            Some("statements[0].actions[0]")
        );
    }

    #[test]
    fn deprecated_action_carries_replacement_hint() {
        // The catalog ships `vault:unseal_history` with replacement
        // `vault:read_metadata` since 0.5.0.
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["vault:unseal_history"],"resources":["vault:secret/foo"]}
        ]}"#;
        let diags = lint(p);
        let d = diags
            .iter()
            .find(|d| d.code == DiagnosticCode::DeprecatedAction)
            .expect("deprecated diagnostic");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.suggested_fix.as_deref(), Some("vault:read_metadata"));
        assert!(d.message.contains("vault:read_metadata"));
        assert!(d.message.contains("0.5.0"));
    }

    #[test]
    fn suspect_resource_triggers_on_bare_star() {
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["*"]}
        ]}"#;
        let diags = lint(p);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, DiagnosticCode::SuspectResource);
        assert_eq!(diags[0].severity, Severity::Warning);
    }

    #[test]
    fn suspect_resource_triggers_on_missing_kind_prefix() {
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["public.orders"]}
        ]}"#;
        let diags = lint(p);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, DiagnosticCode::SuspectResource);
        assert!(diags[0].message.contains("public.orders"));
        assert_eq!(
            diags[0].suggested_fix.as_deref(),
            Some("<kind>:public.orders")
        );
    }

    #[test]
    fn kind_prefixed_glob_resource_is_clean() {
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["table:*"]}
        ]}"#;
        assert!(lint(p).is_empty());
    }

    #[test]
    fn no_effect_triggers_for_overlapping_allow_and_deny() {
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["table:foo"]},
            {"effect":"deny","actions":["select"],"resources":["table:foo"]}
        ]}"#;
        let diags = lint(p);
        let d = diags
            .iter()
            .find(|d| d.code == DiagnosticCode::NoEffectStatements)
            .expect("no-effect diagnostic");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.location.as_deref(), Some("statements[0]"));
    }

    #[test]
    fn no_effect_is_suppressed_when_deny_has_a_condition() {
        // The Deny only fires inside its time window — it does NOT
        // universally shadow the Allow, so the no-effect heuristic
        // must not trigger.
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["table:foo"]},
            {"effect":"deny","actions":["select"],"resources":["table:foo"],
             "condition":{"mfa":true}}
        ]}"#;
        let diags = lint(p);
        assert!(
            !diags
                .iter()
                .any(|d| d.code == DiagnosticCode::NoEffectStatements),
            "{diags:?}"
        );
    }

    #[test]
    fn no_effect_requires_action_overlap() {
        // Allow on `select`, Deny on `insert` — different actions,
        // not a no-op.
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow","actions":["select"],"resources":["table:foo"]},
            {"effect":"deny","actions":["insert"],"resources":["table:foo"]}
        ]}"#;
        let diags = lint(p);
        assert!(
            !diags
                .iter()
                .any(|d| d.code == DiagnosticCode::NoEffectStatements),
            "{diags:?}"
        );
    }

    #[test]
    fn diagnostics_sort_errors_before_warnings() {
        // Mix one Unknown (Error) with one SuspectResource (Warning)
        // and one Deprecated (Warning); error must come first, then
        // warnings in code order.
        let p = r#"{"id":"p","version":1,"statements":[
            {"effect":"allow",
             "actions":["definitely-not-an-action","vault:unseal_history"],
             "resources":["table:foo","*"]}
        ]}"#;
        let diags = lint(p);
        assert!(diags.len() >= 3, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        // Following diagnostics are warnings.
        for d in &diags[1..] {
            assert_eq!(d.severity, Severity::Warning);
        }
    }

    #[test]
    fn invalid_json_returns_single_error_diagnostic() {
        let diags = lint("{ not json");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn self_lock_risk_flags_deny_detach_on_wildcard() {
        let p = r#"{
            "id": "p-brick",
            "version": 1,
            "statements": [{
                "sid": "lock",
                "effect": "deny",
                "actions": ["policy:detach"],
                "resources": ["*"]
            }]
        }"#;
        let diags = lint(p);
        let d = diags
            .iter()
            .find(|d| d.code == DiagnosticCode::SelfLockRisk)
            .expect("self-lock diagnostic");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("self-lock invariant"), "{}", d.message);
        assert!(d.message.contains("p-brick"), "{}", d.message);
        assert!(d.message.contains("lock"), "{}", d.message);
        assert_eq!(d.location.as_deref(), Some("statements[0]"));
    }

    #[test]
    fn self_lock_risk_message_matches_attach_time_error_verbatim() {
        use crate::auth::policies::Policy;
        use crate::auth::self_lock_guard::{check_self_lock_invariant, format_block_error};
        use std::sync::Arc;

        let raw = r#"{
            "id": "p-brick",
            "version": 1,
            "statements": [{
                "sid": "lock",
                "effect": "deny",
                "actions": ["policy:detach"],
                "resources": ["*"]
            }]
        }"#;

        // Linter side.
        let diags = lint(raw);
        let d = diags
            .iter()
            .find(|d| d.code == DiagnosticCode::SelfLockRisk)
            .expect("self-lock diagnostic");

        // Attach-side error built from the same primitive.
        let policy = Arc::new(Policy::from_json_str(raw).expect("parses"));
        let outcome = check_self_lock_invariant(&[policy]);
        let attach_msg = format_block_error(&outcome).expect("blocked carries a message");

        assert_eq!(d.message, attach_msg, "linter must mirror attach error");
    }

    #[test]
    fn self_lock_risk_silent_for_narrower_deny() {
        // The same deny restricted to tenant-scoped principals does
        // not lock the synthetic platform owner out.
        let p = r#"{
            "id": "p-narrow",
            "version": 1,
            "statements": [{
                "effect": "deny",
                "actions": ["policy:detach"],
                "resources": ["*"],
                "condition": { "platform_scoped": false }
            }]
        }"#;
        let diags = lint(p);
        assert!(
            !diags.iter().any(|d| d.code == DiagnosticCode::SelfLockRisk),
            "{diags:?}"
        );
    }

    #[test]
    fn self_lock_risk_silent_for_clean_policy() {
        assert!(!lint(clean_policy())
            .iter()
            .any(|d| d.code == DiagnosticCode::SelfLockRisk));
    }

    #[test]
    fn diagnostic_json_includes_all_fields() {
        let d = Diagnostic {
            severity: Severity::Warning,
            code: DiagnosticCode::DeprecatedAction,
            message: "test".into(),
            suggested_fix: Some("vault:read".into()),
            location: Some("statements[0].actions[0]".into()),
        };
        let s = d.to_json_value().to_string_compact();
        assert!(s.contains("\"severity\":\"warning\""), "{s}");
        assert!(s.contains("\"code\":\"deprecated_action\""), "{s}");
        assert!(s.contains("\"suggested_fix\":\"vault:read\""), "{s}");
        assert!(
            s.contains("\"location\":\"statements[0].actions[0]\""),
            "{s}"
        );
    }
}
