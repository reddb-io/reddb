//! Issue #789 — Enforce Analytics v0 non-goals in the parser, with
//! clear v0-specific error messages.
//!
//! Parent PRD #782 ringfences Analytics v0 around a metric-centric
//! catalog and explicitly excludes a handful of surfaces:
//!
//! * `CREATE ANALYTICS …` — no generic analytics object.
//! * `CREATE EVENT …` — no new event storage model; events live in
//!   ordinary TABLE/DOCUMENT collections.
//! * `INSERT INTO METRIC …` — raw writes go to ordinary collections,
//!   not directly to the metric catalog.
//! * `CREATE COHORT …` / `CREATE FUNNEL …` — cohort/funnel APIs are
//!   deferred.
//! * `CREATE SLA …` — SLA/legal contract modeling is post-MVP.
//! * `CREATE ADAPTER …` — Prometheus/Grafana/Snowplow/GA adapters are
//!   deferred.
//!
//! Each form must surface a stable, v0-scoped rejection at parse time
//! so accidental use does not silently take a different path (e.g.
//! `INSERT INTO METRIC` falling through to the generic identifier
//! slot and reporting "expected identifier", which would obscure the
//! v0 boundary).
//!
//! Sibling slices in #784 / #785 keep `CREATE METRIC` and
//! `SELECT … FROM red.analytics.metrics` working — this suite only
//! pins what is **out** of scope.

mod support;

use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn err_for(rt: &RedDBRuntime, sql: &str) -> String {
    match rt.execute_query(sql) {
        Ok(_) => panic!("expected v0 non-goal rejection, got success: {sql}"),
        Err(err) => format!("{err}"),
    }
}

#[test]
fn create_analytics_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE ANALYTICS product");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE ANALYTICS"),
        "expected v0 non-goal message naming CREATE ANALYTICS, got: {msg}"
    );
}

#[test]
fn create_event_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE EVENT app_opened");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE EVENT"),
        "expected v0 non-goal message naming CREATE EVENT, got: {msg}"
    );
    // The message must steer the user to the supported path —
    // ordinary TABLE/DOCUMENT collections — so the boundary is
    // actionable, not just a refusal.
    assert!(
        msg.to_ascii_uppercase().contains("TABLE") || msg.to_ascii_uppercase().contains("DOCUMENT"),
        "expected message to point at TABLE/DOCUMENT collections, got: {msg}"
    );
}

#[test]
fn create_cohort_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE COHORT power_users");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE COHORT"),
        "expected v0 non-goal message naming CREATE COHORT, got: {msg}"
    );
}

#[test]
fn create_funnel_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE FUNNEL signup_to_activation");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE FUNNEL"),
        "expected v0 non-goal message naming CREATE FUNNEL, got: {msg}"
    );
}

#[test]
fn create_sla_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE SLA enterprise_uptime");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE SLA"),
        "expected v0 non-goal message naming CREATE SLA, got: {msg}"
    );
}

#[test]
fn create_adapter_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    let msg = err_for(&rt, "CREATE ADAPTER prometheus");
    assert!(
        msg.contains("Analytics v0") && msg.contains("CREATE ADAPTER"),
        "expected v0 non-goal message naming CREATE ADAPTER, got: {msg}"
    );
}

#[test]
fn insert_into_metric_is_rejected_as_v0_non_goal() {
    let rt = runtime();
    // The form the parser must reject explicitly. Without this rule,
    // the lexer's `Token::Metric` keyword would fail the generic
    // `expected identifier` slot, hiding the v0 boundary behind a
    // confusing grammar message.
    let msg = err_for(
        &rt,
        "INSERT INTO METRIC infra.database.cpu.usage \
         (ts, value) VALUES (1, 0.5)",
    );
    assert!(
        msg.contains("Analytics v0") && msg.contains("INSERT INTO METRIC"),
        "expected v0 non-goal message naming INSERT INTO METRIC, got: {msg}"
    );
    // The message must point the caller at the supported path:
    // raw writes go to ordinary collections; the catalog is reached
    // through CREATE METRIC / red.analytics.metrics.
    assert!(
        msg.contains("CREATE METRIC") || msg.contains("red.analytics.metrics"),
        "expected message to steer to the supported metric path, got: {msg}"
    );
}

#[test]
fn supported_v0_metric_path_is_unaffected_by_non_goal_rules() {
    // Regression guard: the non-goal rejections sit next to the
    // supported `CREATE METRIC <path> TYPE <kind> ROLE <role>` form
    // (issue #784) and the `red.analytics.metrics` read surface
    // (issue #785). Both must still work — the non-goal arms only
    // intercept the explicitly-excluded keywords.
    let rt = runtime();
    rt.execute_query("CREATE METRIC infra.database.cpu.usage TYPE gauge ROLE operational")
        .expect("CREATE METRIC must remain supported in v0");
    let result = rt
        .execute_query(
            "SELECT path FROM red.analytics.metrics \
             WHERE path = 'infra.database.cpu.usage'",
        )
        .expect("red.analytics.metrics catalog read must remain supported");
    assert_eq!(result.result.records.len(), 1);
}

#[test]
fn unrelated_create_form_still_falls_through_to_generic_error() {
    // Regression guard: the non-goal map is an allow-list of specific
    // keywords; any other unknown CREATE form must still hit the
    // generic CREATE fallback so the grammar surface stays predictable.
    let rt = runtime();
    let msg = err_for(&rt, "CREATE FROBNICATOR foo");
    assert!(
        !msg.contains("Analytics v0"),
        "unrelated CREATE form should not be tagged Analytics v0, got: {msg}"
    );
}
