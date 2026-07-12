//! Red UI cluster status snapshot (#738).
//!
//! Aggregates deployment shape, runtime health, storage, WAL, system
//! resources, and replication facts behind one stable contract so the
//! UI can render a cluster status page from a single payload.
//!
//! Per the #738 thread-discussion decision, telemetry RedDB cannot
//! reliably measure today is exposed as a structured
//! `{ "available": false, "reason": "..." }` object — never fabricated
//! and never silently absent. Renderers know the field exists and why
//! it is empty without probing every other endpoint.
//!
//! The builder is a pure function over `ClusterStatusInputs` so
//! handler wiring is trivial and contract tests can pin shape against
//! synthetic fixtures (standalone, degraded primary, ...).
//!
//! ## Honesty rules
//!
//! * Fields whose value the engine *cannot* observe in this build are
//!   `{ "available": false, "reason": "<short-token>" }`. Examples:
//!   CPU%, RAM%, WAL bytes, request throughput / latency aggregates,
//!   process container metadata, last-error capture — see #738 brief.
//! * Fields the engine *can* observe but which are 0/missing for a
//!   legitimate reason (e.g. `replica_count = 0` on a standalone)
//!   keep their natural type, not `unavailable`. Absence of data is
//!   different from inability to measure.
//! * `replication.degraded` is `true` when apply health is one of
//!   `stalled_gap`, `divergence`, `apply_error`. `false` on healthy /
//!   standalone. `"unknown"` only when the apply pump has not run
//!   yet (replica fresh attach with no health label).

use crate::json::{Map, Value as JsonValue};
use crate::runtime::node_load_telemetry::NodeLoadSnapshot;

/// What the snapshot can see at one moment in time.
///
/// All fields are either captured values or `None` to mean
/// "engine cannot measure". The presentation layer turns `None` into
/// an `unavailable` envelope with a stable reason so the UI never
/// sees a fabricated default.
#[derive(Debug, Clone)]
pub(crate) struct ClusterStatusInputs {
    pub(crate) snapshot_at_unix_ms: u64,
    pub(crate) version: String,
    pub(crate) phase: String,
    pub(crate) uptime_secs: f64,
    pub(crate) started_at_unix_ms: u64,
    pub(crate) ready_at_unix_ms: Option<u64>,
    pub(crate) read_only: bool,

    pub(crate) deployment_shape: DeploymentShapeView,
    pub(crate) process_role: ProcessRoleView,

    pub(crate) transport: TransportSnapshot,

    pub(crate) connections: ConnectionSnapshot,

    pub(crate) storage: StorageSnapshot,

    pub(crate) wal: WalSnapshot,

    pub(crate) system: SystemSnapshot,

    pub(crate) replication: ReplicationSnapshot,

    /// Query latency percentiles derived from the recorded histogram
    /// substrate (#1241). `None` when no query has been sampled yet —
    /// the field stays an honest `unavailable` envelope (§6) rather than
    /// reporting a fabricated zero.
    pub(crate) latency: Option<LatencySample>,

    /// Per-node load signals (#1245): active query gauge + connection churn
    /// counters. `None` when no connection has been seen yet — the field
    /// stays an honest `unavailable` envelope (§6).
    pub(crate) load: Option<NodeLoadSnapshot>,
}

/// Overall query latency percentiles for `/cluster/status`, derived from
/// the cross-kind histogram rollup. Seconds.
#[derive(Debug, Clone)]
pub(crate) struct LatencySample {
    pub(crate) p50_seconds: f64,
    pub(crate) p95_seconds: f64,
    pub(crate) p99_seconds: f64,
    /// Number of samples behind the percentiles, so the UI can show how
    /// much the window is backed by.
    pub(crate) sample_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeploymentShapeView {
    Embedded,
    File,
    Server,
    Serverless,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessRoleView {
    Standalone,
    Primary,
    Replica,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TransportListenerView {
    pub(crate) transport: String,
    pub(crate) bind_addr: String,
    pub(crate) explicit: bool,
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TransportSnapshot {
    pub(crate) active: Vec<TransportListenerView>,
    pub(crate) failed: Vec<TransportListenerView>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectionSnapshot {
    pub(crate) active: u64,
    pub(crate) idle: u64,
    pub(crate) total_checkouts: u64,
    pub(crate) max: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct StorageSnapshot {
    /// `Some(bytes)` for file-backed runtimes where `fs::metadata`
    /// returned a length, `None` for remote / in-memory or when the
    /// runtime has no on-disk file at all.
    pub(crate) db_size_bytes: Option<u64>,
    /// Remote backend identifier (`s3`, `fs`, `http`, ...) when one
    /// is configured. `None` for purely local storage.
    pub(crate) remote_backend: Option<String>,
    pub(crate) encryption_state: String,
    pub(crate) encryption_error: Option<String>,
    pub(crate) paged_mode: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WalSnapshot {
    pub(crate) current_lsn: u64,
    pub(crate) last_archived_lsn: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SystemSnapshot {
    pub(crate) pid: u32,
    pub(crate) cpu_cores: usize,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) hostname: String,
    /// `Some(bytes)` only on platforms where the engine reads it
    /// reliably (Linux). `None` everywhere else — the JSON envelope
    /// will mark it `unavailable` with a `reason` rather than report
    /// `0`.
    pub(crate) total_memory_bytes: Option<u64>,
    pub(crate) available_memory_bytes: Option<u64>,
    /// Issue #1244 — whole-node CPU utilisation occupancy. `Measured` once
    /// two samples establish a delta; `NotSampled` on a supported platform
    /// before then; `Unsupported` where the engine cannot probe it.
    pub(crate) cpu_usage: OccupancyView,
    /// Issue #1244 — whole-node RAM utilisation occupancy (used/total).
    pub(crate) ram_usage: OccupancyView,
}

/// Presentation-side view of a node occupancy gauge (#1244). Mirrors the
/// runtime sampler's three honest states without coupling the presentation
/// layer to the sampler type; the handler maps one to the other.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum OccupancyView {
    /// Measured utilisation ratio in `0.0..=1.0`.
    Measured(f64),
    /// Supported platform, but no measured value yet (CPU needs a delta).
    /// The honest default — a snapshot built without an occupancy reading
    /// reports "not sampled", never a fabricated zero.
    #[default]
    NotSampled,
    /// Platform cannot measure this field.
    Unsupported,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplicationSnapshot {
    pub(crate) role: ProcessRoleView,
    pub(crate) commit_policy: String,
    pub(crate) replicas: Vec<ReplicaView>,
    /// Replica-side apply pump health label. `None` on standalone /
    /// primary or when the apply pump has not run yet — the JSON
    /// envelope distinguishes "ok" / "degraded" / "unknown".
    pub(crate) apply_health: Option<String>,
    /// `(kind, count)` pairs sourced from
    /// `runtime.replica_apply_error_counts()`.
    pub(crate) apply_errors: Vec<(String, u64)>,
    /// Issue #1243 (PRD #1237 Phase B) — primary<->replica reconnects since
    /// process start, from `runtime.replication_reconnects_count()`. Lets
    /// the UI explain link instability instead of a last-error snapshot.
    /// Rendered as a number on a replica; `not_applicable` elsewhere.
    pub(crate) reconnects_total: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplicaView {
    pub(crate) id: String,
    pub(crate) last_acked_lsn: u64,
    pub(crate) last_sent_lsn: u64,
    pub(crate) last_durable_lsn: u64,
    pub(crate) last_seen_at_unix_ms: u128,
    pub(crate) region: Option<String>,
}

/// Render a structured "cannot measure" envelope. Keeping the shape
/// stable lets the UI write one renderer for every unavailable field
/// instead of guessing per-call.
pub(crate) fn unavailable_json(reason: &str) -> JsonValue {
    let mut object = Map::new();
    object.insert("available".to_string(), JsonValue::Bool(false));
    object.insert("reason".to_string(), JsonValue::String(reason.to_string()));
    JsonValue::Object(object)
}

/// Render the query latency field (#1241). Honest by construction: with
/// no recorded sample the field is the same `unavailable` envelope it was
/// before this slice (`latency_not_sampled`); once real samples exist it
/// carries the P50/P95/P99 derived from the histogram rollup.
fn latency_json(sample: Option<&LatencySample>) -> JsonValue {
    let Some(sample) = sample else {
        return unavailable_json("latency_not_sampled");
    };
    let round_us = |secs: f64| (secs * 1_000_000.0).round() / 1_000_000.0;
    let mut object = Map::new();
    object.insert("available".to_string(), JsonValue::Bool(true));
    object.insert(
        "p50_seconds".to_string(),
        JsonValue::Number(round_us(sample.p50_seconds)),
    );
    object.insert(
        "p95_seconds".to_string(),
        JsonValue::Number(round_us(sample.p95_seconds)),
    );
    object.insert(
        "p99_seconds".to_string(),
        JsonValue::Number(round_us(sample.p99_seconds)),
    );
    object.insert(
        "sample_count".to_string(),
        JsonValue::Number(sample.sample_count as f64),
    );
    JsonValue::Object(object)
}

/// Render the node load field (#1245). Honest by construction: before any
/// connection is seen the field is an `unavailable` envelope
/// (`load_not_sampled`); once real samples exist it carries the three
/// occupancy signals.
fn load_json(snap: Option<&NodeLoadSnapshot>) -> JsonValue {
    let Some(snap) = snap else {
        return unavailable_json("load_not_sampled");
    };
    let mut object = Map::new();
    object.insert("available".to_string(), JsonValue::Bool(true));
    object.insert(
        "active_queries".to_string(),
        JsonValue::Number(snap.active_queries.max(0) as f64),
    );
    object.insert(
        "connects_total".to_string(),
        JsonValue::Number(snap.connects_total as f64),
    );
    object.insert(
        "disconnects_total".to_string(),
        JsonValue::Number(snap.disconnects_total as f64),
    );
    JsonValue::Object(object)
}

/// Build the `/cluster/status` payload from a captured snapshot.
pub(crate) fn cluster_status_json(inputs: &ClusterStatusInputs) -> JsonValue {
    let mut object = Map::new();

    object.insert(
        "snapshot_at_unix_ms".to_string(),
        JsonValue::Number(inputs.snapshot_at_unix_ms as f64),
    );
    object.insert(
        "version".to_string(),
        JsonValue::String(inputs.version.clone()),
    );
    object.insert("phase".to_string(), JsonValue::String(inputs.phase.clone()));
    object.insert(
        "uptime_secs".to_string(),
        JsonValue::Number((inputs.uptime_secs * 1000.0).round() / 1000.0),
    );
    object.insert(
        "started_at_unix_ms".to_string(),
        JsonValue::Number(inputs.started_at_unix_ms as f64),
    );
    if let Some(ready) = inputs.ready_at_unix_ms {
        object.insert(
            "ready_at_unix_ms".to_string(),
            JsonValue::Number(ready as f64),
        );
    }
    object.insert("read_only".to_string(), JsonValue::Bool(inputs.read_only));

    object.insert("deployment".to_string(), deployment_json(inputs));
    object.insert("transports".to_string(), transport_json(&inputs.transport));
    object.insert(
        "connections".to_string(),
        connections_json(&inputs.connections),
    );
    object.insert("storage".to_string(), storage_json(&inputs.storage));
    object.insert("wal".to_string(), wal_json(&inputs.wal));
    object.insert("system".to_string(), system_json(&inputs.system));

    // Fields that have no carrier in this build go through the
    // honest `unavailable` envelope. Reasons are short stable tokens
    // — change them only if you also change the renderer.
    object.insert(
        "throughput".to_string(),
        unavailable_json("throughput_not_sampled"),
    );
    object.insert("latency".to_string(), latency_json(inputs.latency.as_ref()));
    object.insert("load".to_string(), load_json(inputs.load.as_ref()));
    object.insert(
        "last_error".to_string(),
        unavailable_json("last_error_not_tracked"),
    );

    object.insert(
        "replication".to_string(),
        replication_json(&inputs.replication, inputs.wal.current_lsn),
    );

    JsonValue::Object(object)
}

fn deployment_json(inputs: &ClusterStatusInputs) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "shape".to_string(),
        match inputs.deployment_shape {
            DeploymentShapeView::Embedded => JsonValue::String("embedded".to_string()),
            DeploymentShapeView::File => JsonValue::String("file".to_string()),
            DeploymentShapeView::Server => JsonValue::String("server".to_string()),
            DeploymentShapeView::Serverless => JsonValue::String("serverless".to_string()),
            DeploymentShapeView::Unknown => unavailable_json("deployment_shape_unknown"),
        },
    );
    object.insert(
        "process_role".to_string(),
        match inputs.process_role {
            ProcessRoleView::Standalone => JsonValue::String("standalone".to_string()),
            ProcessRoleView::Primary => JsonValue::String("primary".to_string()),
            ProcessRoleView::Replica => JsonValue::String("replica".to_string()),
            ProcessRoleView::Unknown => unavailable_json("process_role_unknown"),
        },
    );
    // Container / orchestrator metadata is not probed yet — the brief
    // explicitly forbids fabricating it.
    object.insert(
        "container".to_string(),
        unavailable_json("container_metadata_not_probed"),
    );
    JsonValue::Object(object)
}

fn transport_listener_json(listener: &TransportListenerView) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "transport".to_string(),
        JsonValue::String(listener.transport.clone()),
    );
    object.insert(
        "bind_addr".to_string(),
        JsonValue::String(listener.bind_addr.clone()),
    );
    object.insert("explicit".to_string(), JsonValue::Bool(listener.explicit));
    if let Some(reason) = &listener.reason {
        object.insert("reason".to_string(), JsonValue::String(reason.clone()));
    }
    JsonValue::Object(object)
}

fn transport_json(transport: &TransportSnapshot) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "active".to_string(),
        JsonValue::Array(
            transport
                .active
                .iter()
                .map(transport_listener_json)
                .collect(),
        ),
    );
    object.insert(
        "failed".to_string(),
        JsonValue::Array(
            transport
                .failed
                .iter()
                .map(transport_listener_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn connections_json(conn: &ConnectionSnapshot) -> JsonValue {
    let mut object = Map::new();
    object.insert("active".to_string(), JsonValue::Number(conn.active as f64));
    object.insert("idle".to_string(), JsonValue::Number(conn.idle as f64));
    object.insert(
        "total_checkouts".to_string(),
        JsonValue::Number(conn.total_checkouts as f64),
    );
    object.insert(
        "max".to_string(),
        match conn.max {
            Some(n) => JsonValue::Number(n as f64),
            None => unavailable_json("max_connections_unconfigured"),
        },
    );
    JsonValue::Object(object)
}

fn storage_json(storage: &StorageSnapshot) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "db_size_bytes".to_string(),
        match storage.db_size_bytes {
            Some(n) => JsonValue::Number(n as f64),
            None => unavailable_json("db_size_not_file_backed"),
        },
    );
    object.insert(
        "remote_backend".to_string(),
        match &storage.remote_backend {
            Some(name) => JsonValue::String(name.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "paged_mode".to_string(),
        JsonValue::Bool(storage.paged_mode),
    );
    let mut enc = Map::new();
    enc.insert(
        "state".to_string(),
        JsonValue::String(storage.encryption_state.clone()),
    );
    if let Some(err) = &storage.encryption_error {
        enc.insert("error".to_string(), JsonValue::String(err.clone()));
    }
    object.insert("encryption_at_rest".to_string(), JsonValue::Object(enc));
    JsonValue::Object(object)
}

fn wal_json(wal: &WalSnapshot) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "current_lsn".to_string(),
        JsonValue::Number(wal.current_lsn as f64),
    );
    object.insert(
        "last_archived_lsn".to_string(),
        JsonValue::Number(wal.last_archived_lsn as f64),
    );
    object.insert(
        "archive_lag_records".to_string(),
        JsonValue::Number(wal.current_lsn.saturating_sub(wal.last_archived_lsn) as f64),
    );
    object.insert(
        "bytes".to_string(),
        unavailable_json("wal_bytes_not_tracked"),
    );
    JsonValue::Object(object)
}

fn system_json(system: &SystemSnapshot) -> JsonValue {
    let mut object = Map::new();
    object.insert("pid".to_string(), JsonValue::Number(system.pid as f64));
    object.insert(
        "cpu_cores".to_string(),
        JsonValue::Number(system.cpu_cores as f64),
    );
    object.insert("os".to_string(), JsonValue::String(system.os.clone()));
    object.insert("arch".to_string(), JsonValue::String(system.arch.clone()));
    object.insert(
        "hostname".to_string(),
        JsonValue::String(system.hostname.clone()),
    );
    object.insert(
        "total_memory_bytes".to_string(),
        match system.total_memory_bytes {
            Some(n) => JsonValue::Number(n as f64),
            None => unavailable_json("memory_probe_not_supported"),
        },
    );
    object.insert(
        "available_memory_bytes".to_string(),
        match system.available_memory_bytes {
            Some(n) => JsonValue::Number(n as f64),
            None => unavailable_json("memory_probe_not_supported"),
        },
    );
    object.insert(
        "cpu_usage".to_string(),
        occupancy_json(
            system.cpu_usage,
            "cpu_usage_not_sampled",
            "cpu_usage_not_supported",
        ),
    );
    object.insert(
        "ram_usage".to_string(),
        occupancy_json(
            system.ram_usage,
            "ram_usage_not_sampled",
            "ram_usage_not_supported",
        ),
    );
    JsonValue::Object(object)
}

/// Render a node occupancy gauge (#1244). A measured value carries both the
/// raw ratio (`0..=1`) and a convenience percent; the unmeasured states fall
/// through to the stable `unavailable` envelope (§6) with a reason that
/// distinguishes "no sample yet" from "platform cannot measure".
fn occupancy_json(
    view: OccupancyView,
    not_sampled_reason: &str,
    unsupported_reason: &str,
) -> JsonValue {
    match view {
        OccupancyView::Measured(ratio) => {
            let ratio = ratio.clamp(0.0, 1.0);
            // Round the ratio to 4 decimals; percent is derived from the
            // same rounded value so the two never disagree.
            let ratio = (ratio * 10_000.0).round() / 10_000.0;
            let mut object = Map::new();
            object.insert("available".to_string(), JsonValue::Bool(true));
            object.insert("usage_ratio".to_string(), JsonValue::Number(ratio));
            object.insert(
                "usage_percent".to_string(),
                JsonValue::Number((ratio * 1_000.0).round() / 10.0),
            );
            JsonValue::Object(object)
        }
        OccupancyView::NotSampled => unavailable_json(not_sampled_reason),
        OccupancyView::Unsupported => unavailable_json(unsupported_reason),
    }
}

fn replication_json(repl: &ReplicationSnapshot, current_lsn: u64) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "role".to_string(),
        match repl.role {
            ProcessRoleView::Standalone => JsonValue::String("standalone".to_string()),
            ProcessRoleView::Primary => JsonValue::String("primary".to_string()),
            ProcessRoleView::Replica => JsonValue::String("replica".to_string()),
            ProcessRoleView::Unknown => unavailable_json("process_role_unknown"),
        },
    );
    object.insert(
        "commit_policy".to_string(),
        JsonValue::String(repl.commit_policy.clone()),
    );
    object.insert(
        "replica_count".to_string(),
        JsonValue::Number(repl.replicas.len() as f64),
    );
    let replicas = repl
        .replicas
        .iter()
        .map(|r| {
            let mut o = Map::new();
            o.insert("id".to_string(), JsonValue::String(r.id.clone()));
            o.insert(
                "last_acked_lsn".to_string(),
                JsonValue::Number(r.last_acked_lsn as f64),
            );
            o.insert(
                "last_sent_lsn".to_string(),
                JsonValue::Number(r.last_sent_lsn as f64),
            );
            o.insert(
                "last_durable_lsn".to_string(),
                JsonValue::Number(r.last_durable_lsn as f64),
            );
            o.insert(
                "last_seen_at_unix_ms".to_string(),
                JsonValue::Number(r.last_seen_at_unix_ms as f64),
            );
            o.insert(
                "lag_records".to_string(),
                JsonValue::Number(current_lsn.saturating_sub(r.last_acked_lsn) as f64),
            );
            o.insert(
                "region".to_string(),
                match &r.region {
                    Some(reg) => JsonValue::String(reg.clone()),
                    None => JsonValue::Null,
                },
            );
            JsonValue::Object(o)
        })
        .collect();
    object.insert("replicas".to_string(), JsonValue::Array(replicas));

    // apply_health: distinguish "ok", a known-degraded label, and
    // the "no apply pump observed yet" case. Per-thread #738 we will
    // not fabricate a green health on a non-replica process — return
    // `"not_applicable"` so the UI knows the field is intentionally
    // empty rather than measurement-unreachable.
    let (apply_health_json, degraded_json) = match repl.role {
        ProcessRoleView::Replica => match repl.apply_health.as_deref() {
            Some(label) => {
                let degraded = matches!(label, "stalled_gap" | "divergence" | "apply_error");
                (
                    JsonValue::String(label.to_string()),
                    JsonValue::Bool(degraded),
                )
            }
            None => (
                unavailable_json("apply_health_not_observed"),
                unavailable_json("apply_health_not_observed"),
            ),
        },
        ProcessRoleView::Standalone | ProcessRoleView::Primary => (
            JsonValue::String("not_applicable".to_string()),
            JsonValue::Bool(false),
        ),
        ProcessRoleView::Unknown => (
            unavailable_json("process_role_unknown"),
            unavailable_json("process_role_unknown"),
        ),
    };
    object.insert("apply_health".to_string(), apply_health_json);
    object.insert("degraded".to_string(), degraded_json);

    let mut errors_obj = Map::new();
    for (kind, count) in &repl.apply_errors {
        errors_obj.insert(kind.clone(), JsonValue::Number(*count as f64));
    }
    object.insert("apply_errors".to_string(), JsonValue::Object(errors_obj));

    // Issue #1243 — reconnect count. Honest by role: a number on a replica
    // (the loop that produces it ran), `not_applicable` on standalone /
    // primary where there is no upstream link to reconnect to.
    let reconnects_json = match repl.role {
        ProcessRoleView::Replica => JsonValue::Number(repl.reconnects_total as f64),
        ProcessRoleView::Standalone | ProcessRoleView::Primary => {
            JsonValue::String("not_applicable".to_string())
        }
        ProcessRoleView::Unknown => unavailable_json("process_role_unknown"),
    };
    object.insert("reconnects_total".to_string(), reconnects_json);

    JsonValue::Object(object)
}

#[cfg(test)]
mod tests {
    //! #738 — contract tests for the Red UI cluster status snapshot.
    //!
    //! Each test pins the public envelope shape against a synthetic
    //! `ClusterStatusInputs`. The two acceptance scenarios — standalone
    //! and a degraded/partial-data fixture — both live here.
    use super::*;

    fn base_inputs() -> ClusterStatusInputs {
        ClusterStatusInputs {
            snapshot_at_unix_ms: 1_700_000_000_000,
            version: "0.0.0-test".to_string(),
            phase: "ready".to_string(),
            uptime_secs: 12.5,
            started_at_unix_ms: 1_699_999_987_500,
            ready_at_unix_ms: Some(1_699_999_990_000),
            read_only: false,
            deployment_shape: DeploymentShapeView::Server,
            process_role: ProcessRoleView::Standalone,
            transport: TransportSnapshot {
                active: vec![TransportListenerView {
                    transport: "http".to_string(),
                    bind_addr: "127.0.0.1:5000".to_string(),
                    explicit: true,
                    reason: None,
                }],
                failed: vec![],
            },
            connections: ConnectionSnapshot {
                active: 1,
                idle: 4,
                total_checkouts: 17,
                max: Some(50),
            },
            storage: StorageSnapshot {
                db_size_bytes: Some(4096),
                remote_backend: None,
                encryption_state: "off".to_string(),
                encryption_error: None,
                paged_mode: false,
            },
            wal: WalSnapshot {
                current_lsn: 100,
                last_archived_lsn: 100,
            },
            system: SystemSnapshot {
                pid: 4242,
                cpu_cores: 8,
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                hostname: "test-host".to_string(),
                total_memory_bytes: Some(16 * 1024 * 1024 * 1024),
                available_memory_bytes: Some(8 * 1024 * 1024 * 1024),
                cpu_usage: OccupancyView::NotSampled,
                ram_usage: OccupancyView::NotSampled,
            },
            replication: ReplicationSnapshot {
                role: ProcessRoleView::Standalone,
                commit_policy: "local".to_string(),
                replicas: vec![],
                apply_health: None,
                apply_errors: vec![],
                reconnects_total: 0,
            },
            latency: None,
            load: None,
        }
    }

    fn obj<'a>(v: &'a JsonValue) -> &'a Map<String, JsonValue> {
        v.as_object().unwrap()
    }
    fn s<'a>(v: &'a JsonValue) -> &'a str {
        v.as_str().unwrap()
    }
    fn n(v: &JsonValue) -> f64 {
        v.as_f64().unwrap()
    }
    fn unavail_reason<'a>(v: &'a JsonValue) -> &'a str {
        let o = obj(v);
        assert_eq!(o.get("available").and_then(JsonValue::as_bool), Some(false));
        s(o.get("reason").unwrap())
    }

    #[test]
    fn cluster_status_standalone_classifies_server_and_marks_unmeasurable_fields() {
        let json = cluster_status_json(&base_inputs());
        let root = obj(&json);

        // Top-level envelope shape.
        for key in [
            "snapshot_at_unix_ms",
            "version",
            "phase",
            "uptime_secs",
            "started_at_unix_ms",
            "ready_at_unix_ms",
            "read_only",
            "deployment",
            "transports",
            "connections",
            "storage",
            "wal",
            "system",
            "throughput",
            "latency",
            "load",
            "last_error",
            "replication",
        ] {
            assert!(root.contains_key(key), "missing key {key}");
        }

        assert_eq!(s(root.get("phase").unwrap()), "ready");
        assert_eq!(s(root.get("version").unwrap()), "0.0.0-test");

        // Deployment shape + process role are known.
        let dep = obj(root.get("deployment").unwrap());
        assert_eq!(s(dep.get("shape").unwrap()), "server");
        assert_eq!(s(dep.get("process_role").unwrap()), "standalone");
        // Container metadata is honestly unavailable.
        assert_eq!(
            unavail_reason(dep.get("container").unwrap()),
            "container_metadata_not_probed"
        );

        // Transports.
        let tr = obj(root.get("transports").unwrap());
        let active = tr.get("active").and_then(JsonValue::as_array).unwrap();
        assert_eq!(active.len(), 1);
        let listener = obj(&active[0]);
        assert_eq!(s(listener.get("transport").unwrap()), "http");
        assert_eq!(s(listener.get("bind_addr").unwrap()), "127.0.0.1:5000");

        // Connections — measurable values keep their natural type.
        let conn = obj(root.get("connections").unwrap());
        assert_eq!(n(conn.get("active").unwrap()), 1.0);
        assert_eq!(n(conn.get("max").unwrap()), 50.0);

        // Storage — file-backed, encryption_at_rest object present.
        let st = obj(root.get("storage").unwrap());
        assert_eq!(n(st.get("db_size_bytes").unwrap()), 4096.0);
        assert!(matches!(st.get("remote_backend").unwrap(), JsonValue::Null));
        assert_eq!(
            s(obj(st.get("encryption_at_rest").unwrap())
                .get("state")
                .unwrap()),
            "off"
        );

        // WAL — current/last_archived numeric, bytes unavailable.
        let wal = obj(root.get("wal").unwrap());
        assert_eq!(n(wal.get("current_lsn").unwrap()), 100.0);
        assert_eq!(n(wal.get("archive_lag_records").unwrap()), 0.0);
        assert_eq!(
            unavail_reason(wal.get("bytes").unwrap()),
            "wal_bytes_not_tracked"
        );

        // System — total/available memory measurable, cpu/ram usage not.
        let sys = obj(root.get("system").unwrap());
        assert_eq!(n(sys.get("cpu_cores").unwrap()), 8.0);
        assert!(matches!(
            sys.get("total_memory_bytes").unwrap(),
            JsonValue::Number(_) | JsonValue::Integer(_)
        ));
        assert_eq!(
            unavail_reason(sys.get("cpu_usage").unwrap()),
            "cpu_usage_not_sampled"
        );
        assert_eq!(
            unavail_reason(sys.get("ram_usage").unwrap()),
            "ram_usage_not_sampled"
        );

        // Throughput / latency / last_error are honest "unavailable".
        assert_eq!(
            unavail_reason(root.get("throughput").unwrap()),
            "throughput_not_sampled"
        );
        assert_eq!(
            unavail_reason(root.get("latency").unwrap()),
            "latency_not_sampled"
        );
        assert_eq!(
            unavail_reason(root.get("last_error").unwrap()),
            "last_error_not_tracked"
        );

        // Replication — standalone, zero replicas, apply_health
        // "not_applicable" (the role-doesn't-have-an-apply-pump case),
        // degraded=false.
        let repl = obj(root.get("replication").unwrap());
        assert_eq!(s(repl.get("role").unwrap()), "standalone");
        assert_eq!(s(repl.get("commit_policy").unwrap()), "local");
        assert_eq!(n(repl.get("replica_count").unwrap()), 0.0);
        assert!(repl
            .get("replicas")
            .and_then(JsonValue::as_array)
            .unwrap()
            .is_empty());
        assert_eq!(s(repl.get("apply_health").unwrap()), "not_applicable");
        assert_eq!(
            repl.get("degraded").and_then(JsonValue::as_bool),
            Some(false)
        );
    }

    #[test]
    fn cluster_status_degraded_replica_reports_degraded_true_and_unavailable_memory() {
        // Replica with `stalled_gap` apply health, no memory probe
        // (non-linux host), one observed replica peer with lag, and
        // failed transport listener.
        let mut inputs = base_inputs();
        inputs.process_role = ProcessRoleView::Replica;
        inputs.system.total_memory_bytes = None;
        inputs.system.available_memory_bytes = None;
        inputs.system.os = "macos".to_string();
        inputs.transport.failed.push(TransportListenerView {
            transport: "grpc".to_string(),
            bind_addr: "0.0.0.0:5000".to_string(),
            explicit: true,
            reason: Some("port_in_use".to_string()),
        });
        inputs.wal.current_lsn = 200;
        inputs.wal.last_archived_lsn = 150;
        inputs.replication = ReplicationSnapshot {
            role: ProcessRoleView::Replica,
            commit_policy: "ack_n=2".to_string(),
            replicas: vec![ReplicaView {
                id: "replica-a".to_string(),
                last_acked_lsn: 180,
                last_sent_lsn: 195,
                last_durable_lsn: 175,
                last_seen_at_unix_ms: 1_700_000_000_000,
                region: Some("us-east-1".to_string()),
            }],
            apply_health: Some("stalled_gap".to_string()),
            apply_errors: vec![("gap".to_string(), 3), ("apply".to_string(), 0)],
            reconnects_total: 4,
        };

        let json = cluster_status_json(&inputs);
        let root = obj(&json);

        // Deployment / process role reflect the replica.
        let dep = obj(root.get("deployment").unwrap());
        assert_eq!(s(dep.get("process_role").unwrap()), "replica");

        // Transports: failed listener carries a reason.
        let tr = obj(root.get("transports").unwrap());
        let failed = tr.get("failed").and_then(JsonValue::as_array).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(s(obj(&failed[0]).get("reason").unwrap()), "port_in_use");

        // WAL — archive lag is non-zero.
        let wal = obj(root.get("wal").unwrap());
        assert_eq!(n(wal.get("archive_lag_records").unwrap()), 50.0);

        // System — memory probe was unavailable on this host; the
        // envelope reports it as `unavailable`, never `0`.
        let sys = obj(root.get("system").unwrap());
        assert_eq!(
            unavail_reason(sys.get("total_memory_bytes").unwrap()),
            "memory_probe_not_supported"
        );
        assert_eq!(
            unavail_reason(sys.get("available_memory_bytes").unwrap()),
            "memory_probe_not_supported"
        );

        // Replication — degraded apply health, real replicas array
        // with lag, apply_errors map exposes both counters.
        let repl = obj(root.get("replication").unwrap());
        assert_eq!(s(repl.get("role").unwrap()), "replica");
        assert_eq!(s(repl.get("apply_health").unwrap()), "stalled_gap");
        assert_eq!(
            repl.get("degraded").and_then(JsonValue::as_bool),
            Some(true)
        );
        let replicas = repl.get("replicas").and_then(JsonValue::as_array).unwrap();
        assert_eq!(replicas.len(), 1);
        let r0 = obj(&replicas[0]);
        assert_eq!(s(r0.get("id").unwrap()), "replica-a");
        // lag_records = current_lsn - last_acked_lsn = 200 - 180 = 20.
        assert_eq!(n(r0.get("lag_records").unwrap()), 20.0);
        assert_eq!(s(r0.get("region").unwrap()), "us-east-1");
        let errs = obj(repl.get("apply_errors").unwrap());
        assert_eq!(n(errs.get("gap").unwrap()), 3.0);
        assert_eq!(n(errs.get("apply").unwrap()), 0.0);
        // Issue #1243 — reconnect count rendered as a number on a replica.
        assert_eq!(n(repl.get("reconnects_total").unwrap()), 4.0);
    }

    #[test]
    fn cluster_status_unknown_role_marks_role_and_apply_health_unavailable() {
        // Defensive: if the runtime cannot classify the process role
        // (future "unknown" state) the envelope must surface
        // `unavailable` for both `role` and `apply_health`, not silently
        // default to "standalone".
        let mut inputs = base_inputs();
        inputs.process_role = ProcessRoleView::Unknown;
        inputs.replication.role = ProcessRoleView::Unknown;
        inputs.replication.apply_health = None;

        let json = cluster_status_json(&inputs);
        let repl = obj(obj(&json).get("replication").unwrap());
        assert_eq!(
            unavail_reason(repl.get("role").unwrap()),
            "process_role_unknown"
        );
        assert_eq!(
            unavail_reason(repl.get("apply_health").unwrap()),
            "process_role_unknown"
        );
        assert_eq!(
            unavail_reason(repl.get("degraded").unwrap()),
            "process_role_unknown"
        );
    }

    #[test]
    fn latency_flips_from_unavailable_to_percentiles_once_sampled() {
        // #1241 — with no sample the field stays the honest envelope.
        let json = cluster_status_json(&base_inputs());
        assert_eq!(
            unavail_reason(obj(&json).get("latency").unwrap()),
            "latency_not_sampled"
        );

        // With a recorded sample it carries P50/P95/P99 + count.
        let mut inputs = base_inputs();
        inputs.latency = Some(LatencySample {
            p50_seconds: 0.01,
            p95_seconds: 0.2,
            p99_seconds: 0.9,
            sample_count: 100,
        });
        let json = cluster_status_json(&inputs);
        let lat = obj(obj(&json).get("latency").unwrap());
        assert_eq!(
            lat.get("available").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(n(lat.get("p50_seconds").unwrap()), 0.01);
        assert_eq!(n(lat.get("p95_seconds").unwrap()), 0.2);
        assert_eq!(n(lat.get("p99_seconds").unwrap()), 0.9);
        assert_eq!(n(lat.get("sample_count").unwrap()), 100.0);
    }

    #[test]
    fn occupancy_measured_carries_ratio_and_percent() {
        // #1244 — supported platform with real samples: cpu/ram flip from
        // the honest envelope to measured values.
        let mut inputs = base_inputs();
        inputs.system.cpu_usage = OccupancyView::Measured(0.4237);
        inputs.system.ram_usage = OccupancyView::Measured(0.75);
        let json = cluster_status_json(&inputs);
        let sys = obj(obj(&json).get("system").unwrap());

        let cpu = obj(sys.get("cpu_usage").unwrap());
        assert_eq!(
            cpu.get("available").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(n(cpu.get("usage_ratio").unwrap()), 0.4237);
        assert_eq!(n(cpu.get("usage_percent").unwrap()), 42.4);

        let ram = obj(sys.get("ram_usage").unwrap());
        assert_eq!(
            ram.get("available").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(n(ram.get("usage_ratio").unwrap()), 0.75);
        assert_eq!(n(ram.get("usage_percent").unwrap()), 75.0);
    }

    #[test]
    fn occupancy_not_sampled_is_honest_envelope() {
        // #1244 — supported platform, no measured value yet (CPU needs a
        // delta; the no-sample-yet branch).
        let json = cluster_status_json(&base_inputs());
        let sys = obj(obj(&json).get("system").unwrap());
        assert_eq!(
            unavail_reason(sys.get("cpu_usage").unwrap()),
            "cpu_usage_not_sampled"
        );
        assert_eq!(
            unavail_reason(sys.get("ram_usage").unwrap()),
            "ram_usage_not_sampled"
        );
    }

    #[test]
    fn occupancy_unsupported_platform_is_honest_envelope() {
        // #1244 — platform that cannot probe occupancy keeps the
        // `{ available: false, reason }` envelope with a distinct reason.
        let mut inputs = base_inputs();
        inputs.system.cpu_usage = OccupancyView::Unsupported;
        inputs.system.ram_usage = OccupancyView::Unsupported;
        let json = cluster_status_json(&inputs);
        let sys = obj(obj(&json).get("system").unwrap());
        assert_eq!(
            unavail_reason(sys.get("cpu_usage").unwrap()),
            "cpu_usage_not_supported"
        );
        assert_eq!(
            unavail_reason(sys.get("ram_usage").unwrap()),
            "ram_usage_not_supported"
        );
    }

    #[test]
    fn load_flips_from_unavailable_to_counters_once_connected() {
        // #1245 — with no activity the field stays the honest envelope.
        let json = cluster_status_json(&base_inputs());
        assert_eq!(
            unavail_reason(obj(&json).get("load").unwrap()),
            "load_not_sampled"
        );

        // With recorded connects/disconnects + active queries it carries
        // the three occupancy signals.
        let mut inputs = base_inputs();
        inputs.load = Some(NodeLoadSnapshot {
            active_queries: 3,
            connects_total: 10,
            disconnects_total: 7,
        });
        let json = cluster_status_json(&inputs);
        let load = obj(obj(&json).get("load").unwrap());
        assert_eq!(
            load.get("available").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(n(load.get("active_queries").unwrap()), 3.0);
        assert_eq!(n(load.get("connects_total").unwrap()), 10.0);
        assert_eq!(n(load.get("disconnects_total").unwrap()), 7.0);
    }

    #[test]
    fn load_clamps_negative_active_queries_to_zero() {
        let mut inputs = base_inputs();
        inputs.load = Some(NodeLoadSnapshot {
            active_queries: -1,
            connects_total: 5,
            disconnects_total: 5,
        });
        let json = cluster_status_json(&inputs);
        let load = obj(obj(&json).get("load").unwrap());
        assert_eq!(
            n(load.get("active_queries").unwrap()),
            0.0,
            "transient negative gauge must clamp to zero at the presentation layer"
        );
    }

    #[test]
    fn unavailable_envelope_is_stable() {
        let v = unavailable_json("foo");
        let o = obj(&v);
        assert_eq!(o.get("available").and_then(JsonValue::as_bool), Some(false));
        assert_eq!(s(o.get("reason").unwrap()), "foo");
        assert_eq!(o.len(), 2);
    }
}
