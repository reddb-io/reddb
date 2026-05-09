//! ColumnPolicyGate select hot-path benchmark.
//!
//! Run with:
//!   cargo bench -p reddb-server --bench column_policy_gate_bench
//!
//! This harness keeps the workload intentionally small: the baseline is the
//! existing table-level IAM policy evaluation, and the measured cells add the
//! resolved projection check performed by ColumnPolicyGate.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use reddb_server::auth::policies::{evaluate, EvalContext, Policy, ResourceRef};
use reddb_server::auth::{ColumnAccessRequest, ColumnPolicyGate};

fn parse_policy(json: &str) -> Policy {
    Policy::from_json_str(json)
        .unwrap_or_else(|err| panic!("bench policy should parse: {err}; body={json}"))
}

fn select_policy_set() -> Vec<Policy> {
    vec![
        parse_policy(
            r#"{
                "id": "bench-allow-users",
                "version": 1,
                "statements": [{
                    "effect": "allow",
                    "actions": ["select"],
                    "resources": ["table:users"]
                }]
            }"#,
        ),
        parse_policy(
            r#"{
                "id": "bench-deny-unrelated",
                "version": 1,
                "statements": [{
                    "effect": "deny",
                    "actions": ["select"],
                    "resources": ["column:audit_logs.payload"]
                }]
            }"#,
        ),
    ]
}

fn bench_select_hot_path(c: &mut Criterion) {
    let policies = select_policy_set();
    let refs: Vec<&Policy> = policies.iter().collect();
    let ctx = EvalContext::default();
    let table_resource = ResourceRef::new("table", "users");
    let one_column = ColumnAccessRequest::select("users", ["id"]);
    let four_columns = ColumnAccessRequest::select("users", ["id", "name", "email", "created_at"]);
    let gate = ColumnPolicyGate::new(&refs);

    let mut group = c.benchmark_group("column-policy-gate-select-hot-path");
    group.throughput(Throughput::Elements(1));

    group.bench_function("table-auth-only", |b| {
        b.iter(|| {
            black_box(evaluate(
                black_box(&refs),
                black_box("select"),
                black_box(&table_resource),
                black_box(&ctx),
            ))
        });
    });

    group.bench_function("column-gate-1-column", |b| {
        b.iter(|| {
            let outcome = gate.evaluate(black_box(&one_column), black_box(&ctx));
            black_box(outcome.allowed())
        });
    });

    group.bench_function("column-gate-4-columns", |b| {
        b.iter(|| {
            let outcome = gate.evaluate(black_box(&four_columns), black_box(&ctx));
            black_box(outcome.allowed())
        });
    });

    group.finish();
}

criterion_group!(benches, bench_select_hot_path);
criterion_main!(benches);
