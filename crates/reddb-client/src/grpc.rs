//! gRPC backend — wraps the workspace-internal connector
//! ([`crate::connector::RedDBClient`]) under the `grpc` feature.
//!
//! Design note: the connector itself is engine-free (tonic-only),
//! but the published `grpc` feature still pulls `reddb` as an
//! `optional` dep so downstream callers can use `JsonValue` shapes
//! interchangeably between embedded and remote builds. A leaner
//! proto-only path is tracked in `PLAN_DRIVERS.md`.
//!
//! All methods are genuinely async — they `.await` directly on tonic
//! futures. Callers must be in a tokio runtime (any runtime actually,
//! as long as tonic's transport stack is happy there). This crate
//! does not spin up its own runtime.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use crate::connector::RedDBClient;

use crate::error::{ClientError, ErrorCode, Result};
use crate::params::Value as ParamValue;
use crate::router::{ClusterMembership, HealthAwareRouter, Outcome};
use crate::types::{InsertResult, JsonValue, QueryResult, ValueOut};

/// Default per-endpoint pool size when callers don't specify one.
/// Each pooled `RedDBClient` is a clone of the same tonic channel,
/// so this controls client-side dispatch parallelism, not the
/// number of TCP connections (tonic multiplexes internally).
pub const DEFAULT_POOL_SIZE: usize = 4;

/// Async handle to a remote RedDB server over gRPC.
///
/// Internally either a single-endpoint client or a primary +
/// read-replica cluster. Writes always go to the primary; reads
/// round-robin across the replicas (or the primary when the replica
/// pool is empty / `force_primary` is set).
pub struct GrpcClient {
    primary: Endpoint,
    /// Replica endpoints. Wrapped in `RwLock` so topology discovery
    /// (issue #168 / #172) can swap in new replicas at runtime
    /// without rebuilding the whole client.
    replicas: RwLock<Vec<Endpoint>>,
    /// Round-robin counter for replica selection. Wraps cleanly
    /// (`Relaxed` is fine — exact ordering doesn't matter; spreading
    /// load across replicas does).
    ///
    /// Retained as a fallback for the all-equal-weight cold-start
    /// path; the primary selection logic now lives in
    /// [`crate::router::HealthAwareRouter`].
    #[allow(dead_code)]
    next_replica: AtomicUsize,
    /// `?route=primary` opt-out. When true, every operation hits the
    /// primary regardless of method type.
    force_primary: bool,
    /// Per-endpoint pool size, threaded through topology discovery so
    /// newly-discovered replicas inherit the same `Endpoint::connect`
    /// pool size the original cluster was built with.
    pool_size: usize,
    /// Health-aware routing state (issue #171). Replaces the dumb
    /// modulo round-robin with EWMA-RTT + circuit breaker per
    /// endpoint. Behind an `RwLock` so [`update_membership`] can
    /// swap the membership snapshot without poisoning hot reads.
    router: RwLock<HealthAwareRouter>,
}

/// One remote endpoint plus a fixed pool of `RedDBClient` clones.
///
/// Each call picks `pool[next.fetch_add(1) % pool.len()]` and
/// dispatches against a fresh clone of that slot. Tonic clients are
/// cheap to clone (just an `Arc`-bumped channel handle), so the per-
/// call clone is effectively free; the pool gives N-way client-side
/// parallelism that the legacy `Mutex<RedDBClient>` couldn't.
struct Endpoint {
    url: String,
    pool: Vec<RedDBClient>,
    next: AtomicUsize,
}

impl Endpoint {
    async fn connect(url: String, pool_size: usize) -> Result<Self> {
        // `pool_size == 0` is a misconfiguration; clamp to 1 so we
        // still return a working client (matches the legacy single-
        // mutex path).
        let n = pool_size.max(1);
        let head = RedDBClient::connect(&url, None)
            .await
            .map_err(|e| ClientError::new(ErrorCode::IoError, format!("connect {url}: {e}")))?;
        let mut pool = Vec::with_capacity(n);
        for _ in 0..(n - 1) {
            pool.push(head.clone());
        }
        pool.push(head);
        Ok(Self {
            url,
            pool,
            next: AtomicUsize::new(0),
        })
    }

    /// Round-robin pick + clone. Returns an owned `RedDBClient` so
    /// callers can `&mut` it without holding any lock.
    fn pick(&self) -> RedDBClient {
        // Length is >= 1 by construction (see `connect`).
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.pool.len();
        self.pool[idx].clone()
    }
}

impl std::fmt::Debug for GrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let replicas_guard = self.replicas.read().unwrap();
        let replicas: Vec<&str> = replicas_guard.iter().map(|e| e.url.as_str()).collect();
        f.debug_struct("GrpcClient")
            .field("primary", &self.primary.url)
            .field("replicas", &replicas)
            .field("force_primary", &self.force_primary)
            .finish()
    }
}

impl GrpcClient {
    /// Single-host gRPC client. Equivalent to
    /// `connect_cluster(endpoint, &[], false)` with the default pool
    /// size.
    pub async fn connect(endpoint: String) -> Result<Self> {
        Self::connect_with_pool_size(endpoint, DEFAULT_POOL_SIZE).await
    }

    /// Single-host gRPC client with an explicit per-endpoint pool
    /// size. `pool_size = 1` is kept as a sanity fallback that
    /// disables the round-robin pool (one client clone per
    /// endpoint); the underlying tonic channel still multiplexes
    /// requests, so it behaves like the legacy single-channel path
    /// without the legacy `Mutex` serialization.
    pub async fn connect_with_pool_size(endpoint: String, pool_size: usize) -> Result<Self> {
        let primary = Endpoint::connect(endpoint, pool_size).await?;
        let membership = ClusterMembership::new(primary.url.clone(), Vec::new());
        let router = RwLock::new(HealthAwareRouter::with_force_primary(membership, true));
        Ok(Self {
            primary,
            replicas: RwLock::new(Vec::new()),
            next_replica: AtomicUsize::new(0),
            force_primary: true,
            pool_size,
            router,
        })
    }

    /// Multi-host gRPC client. Writes go to `primary`; reads
    /// round-robin across `replicas` unless `force_primary` is set
    /// (equivalent to passing `?route=primary` in the URI).
    pub async fn connect_cluster(
        primary: String,
        replicas: Vec<String>,
        force_primary: bool,
    ) -> Result<Self> {
        Self::connect_cluster_with_pool_size(primary, replicas, force_primary, DEFAULT_POOL_SIZE)
            .await
    }

    /// Multi-host gRPC client with an explicit per-endpoint pool
    /// size. The same `pool_size` is applied to every endpoint
    /// (primary + replicas).
    pub async fn connect_cluster_with_pool_size(
        primary: String,
        replicas: Vec<String>,
        force_primary: bool,
        pool_size: usize,
    ) -> Result<Self> {
        let primary_ep = Endpoint::connect(primary, pool_size).await?;
        let mut replica_eps = Vec::with_capacity(replicas.len());
        for url in replicas {
            replica_eps.push(Endpoint::connect(url, pool_size).await?);
        }
        let membership = ClusterMembership::new(
            primary_ep.url.clone(),
            replica_eps.iter().map(|e| e.url.clone()).collect(),
        );
        let router = RwLock::new(HealthAwareRouter::with_force_primary(
            membership,
            force_primary,
        ));
        Ok(Self {
            primary: primary_ep,
            replicas: RwLock::new(replica_eps),
            next_replica: AtomicUsize::new(0),
            force_primary,
            pool_size,
            router,
        })
    }

    /// Diagnostic: primary URL.
    pub fn endpoint(&self) -> &str {
        &self.primary.url
    }

    /// Diagnostic: replica URLs in declaration order. Cloned because
    /// the inner pool sits behind an `RwLock` that we don't want to
    /// hand out a borrow of.
    pub fn replica_endpoints(&self) -> Vec<String> {
        self.replicas
            .read()
            .unwrap()
            .iter()
            .map(|e| e.url.clone())
            .collect()
    }

    /// Pick a read-side connector clone. Delegates to
    /// [`HealthAwareRouter`] (issue #171): inverse-RTT weighted across
    /// healthy replicas, fallback to primary when all are unhealthy or
    /// `force_primary` is set. Returns the connector clone plus the
    /// index the router used so the caller can `observe(...)` the
    /// outcome. We hand back an owned `RedDBClient` rather than an
    /// `Endpoint` borrow so the read lock on the (now-mutable)
    /// replica pool can drop before the RPC awaits.
    fn read_endpoint(&self) -> (RedDBClient, usize) {
        let idx = self.router.read().unwrap().pick_read_index();
        if idx == 0 {
            return (self.primary.pick(), 0);
        }
        // Index `i` (1-based) maps onto replica `i-1`. Guard against a
        // stale router pointing past the current pool — fall back to
        // primary.
        let replicas = self.replicas.read().unwrap();
        match replicas.get(idx - 1) {
            Some(ep) => (ep.pick(), idx),
            None => (self.primary.pick(), 0),
        }
    }

    /// Refresh routing state from a new cluster membership. Called
    /// by Lane P's TopologyConsumer when it observes a topology
    /// delta.
    pub fn update_membership(&self, new_membership: ClusterMembership) {
        self.router
            .write()
            .unwrap()
            .update_membership(new_membership);
    }

    /// Refresh the live replica pool from a topology advertisement.
    ///
    /// Walks `addrs` (the canonical "primary-first, then replicas"
    /// order from `crate::topology::ClusterMembership`), opens any
    /// new endpoints that aren't already in the pool, and drops the
    /// ones that disappeared. Existing endpoints keep their pool +
    /// connection state untouched. The router is then updated in
    /// lockstep so `pick_read_index()` only returns indices that map
    /// onto live `Endpoint`s.
    ///
    /// This is the only hook the topology refresh loop needs: the
    /// caller decodes `TopologyReply.topology_bytes` via
    /// [`crate::topology::TopologyConsumer`] and hands the resulting
    /// `(primary_addr, replica_addrs)` straight in here.
    pub async fn apply_topology(&self, primary_addr: &str, replica_addrs: &[String]) -> Result<()> {
        // Reject a primary swap — that would invalidate the writer
        // path and is out of scope for this slice. ADR 0008 §2's
        // "advertised primary always wins" still holds for the URI
        // seed; cross-session primary failover is tracked separately.
        if primary_addr != self.primary.url {
            return Err(ClientError::new(
                ErrorCode::InvalidUri,
                format!(
                    "topology advertised primary {} differs from connected {}; primary failover is out of scope for #172",
                    primary_addr, self.primary.url
                ),
            ));
        }
        // Snapshot current URLs without holding the lock across the
        // (potentially blocking) `Endpoint::connect` calls.
        let current_urls: Vec<String> = self
            .replicas
            .read()
            .unwrap()
            .iter()
            .map(|e| e.url.clone())
            .collect();

        // Build the new pool: keep existing endpoints, dial new ones.
        let mut next: Vec<Endpoint> = Vec::with_capacity(replica_addrs.len());
        for url in replica_addrs {
            if current_urls.iter().any(|u| u == url) {
                // Existing — move it across by re-acquiring the lock
                // briefly. We swap_remove from the existing pool so
                // we don't drop + reconnect.
                let mut guard = self.replicas.write().unwrap();
                if let Some(pos) = guard.iter().position(|e| e.url == *url) {
                    next.push(guard.swap_remove(pos));
                }
            } else {
                next.push(Endpoint::connect(url.clone(), self.pool_size).await?);
            }
        }
        // Replace the live pool. Anything still left in the old guard
        // is a dropped replica; let it Drop here (closes channels).
        {
            let mut guard = self.replicas.write().unwrap();
            *guard = next;
        }
        // Sync the router's view of membership so its index space
        // matches the pool's index space.
        let membership = ClusterMembership::new(self.primary.url.clone(), replica_addrs.to_vec());
        self.router.write().unwrap().update_membership(membership);
        Ok(())
    }

    /// Fetch a topology snapshot from the primary and apply it via
    /// [`Self::apply_topology`]. Convenience for tests and the
    /// background refresh loop.
    pub async fn refresh_topology(&self) -> Result<()> {
        let mut client = self.primary.pick();
        let bytes = client
            .topology()
            .await
            .map_err(|e| ClientError::new(ErrorCode::IoError, format!("topology rpc: {e}")))?;
        let membership =
            crate::topology::TopologyConsumer::consume_bytes(&bytes, None).map_err(|e| {
                ClientError::new(ErrorCode::QueryError, format!("decode topology: {e}"))
            })?;
        let replicas: Vec<String> = membership.replicas.iter().map(|r| r.addr.clone()).collect();
        self.apply_topology(&membership.primary.addr, &replicas)
            .await
    }

    /// Record an observation against an endpoint by index. Exposed
    /// for Lane P's probe loop and integration tests.
    pub(crate) fn observe(&self, idx: usize, outcome: Outcome) {
        self.router.read().unwrap().observe_index(idx, outcome);
    }

    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        let (mut client, idx) = self.read_endpoint();
        let started = Instant::now();
        let reply = match client.query_reply(sql).await {
            Ok(r) => {
                self.observe(idx, Outcome::Rtt(started.elapsed()));
                r
            }
            Err(e) => {
                // Treat any RPC error as a wire-level failure for
                // the circuit breaker. Tonic does not expose a
                // dedicated timeout variant we can match on without
                // pulling more deps; the breaker's K=3 threshold
                // tolerates the occasional false positive (a
                // QueryError that happens to be application-level).
                self.observe(idx, Outcome::Timeout);
                return Err(ClientError::new(ErrorCode::QueryError, e.to_string()));
            }
        };
        parse_query_json(&reply.result_json)
    }

    pub async fn query_with(&self, sql: &str, params: &[ParamValue]) -> Result<QueryResult> {
        if params.is_empty() {
            return self.query(sql).await;
        }
        let grpc_params = params_to_grpc_values(params);
        let (mut client, idx) = self.read_endpoint();
        let started = Instant::now();
        let reply = match client.query_reply_with_params(sql, grpc_params).await {
            Ok(r) => {
                self.observe(idx, Outcome::Rtt(started.elapsed()));
                r
            }
            Err(e) => {
                self.observe(idx, Outcome::Timeout);
                return Err(ClientError::new(ErrorCode::QueryError, e.to_string()));
            }
        };
        parse_query_json(&reply.result_json)
    }

    pub async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        if payload.as_object().is_none() {
            return Err(ClientError::new(
                ErrorCode::QueryError,
                "insert payload must be a JSON object".to_string(),
            ));
        }
        let json_payload = payload.to_json_string();
        // Writes always go to the primary.
        let mut client = self.primary.pick();
        let reply = client
            .create_row_entity(collection, &json_payload)
            .await
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(InsertResult {
            affected: 1,
            id: Some(reply.id.to_string()),
        })
    }

    pub async fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        let mut encoded = Vec::with_capacity(payloads.len());
        for payload in payloads {
            if payload.as_object().is_none() {
                return Err(ClientError::new(
                    ErrorCode::QueryError,
                    "bulk_insert payloads must be JSON objects".to_string(),
                ));
            }
            encoded.push(payload.to_json_string());
        }
        let mut client = self.primary.pick();
        let reply = client
            .bulk_create_rows(collection, encoded)
            .await
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(reply.count)
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        let id = id.parse::<u64>().map_err(|_| {
            ClientError::new(
                ErrorCode::InvalidUri,
                "id must be a numeric string".to_string(),
            )
        })?;
        let mut client = self.primary.pick();
        client
            .delete_entity(collection, id)
            .await
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(1)
    }

    pub async fn close(&self) -> Result<()> {
        // The tonic channels close when the inner clients drop with
        // `self`. Nothing explicit to do here.
        Ok(())
    }

    /// Topology refresh hook (issue #168, ADR 0008).
    ///
    /// Single integration point between `GrpcClient` and the
    /// [`crate::topology`] deep module. Lane O (#167) wires the
    /// connector's `Topology` RPC; once those bytes land, callers
    /// pass them straight in here and get back a merged
    /// [`crate::topology::ClusterMembership`] ready for
    /// the future `HealthAwareRouter` (lane Q, #171). No routing
    /// changes happen here — this slice only emits the membership
    /// data structure.
    ///
    /// `uri_seed` is the parsed `grpc://primary,replica1,...` host
    /// list from the connection string. It's a hint, not a
    /// constraint: advertised topology wins on every collision
    /// (see [`crate::topology::TopologyConsumer::consume`]).
    ///
    /// Recoverable errors (`UnknownVersion`, `MalformedEnvelope`)
    /// are surfaced typed; the caller is expected to log a one-line
    /// warning and fall back to URI-only routing per ADR 0008 §4.
    pub fn ingest_topology_bytes(
        &self,
        bytes: &[u8],
        uri_seed: Option<crate::topology::UriSeed>,
    ) -> std::result::Result<crate::topology::ClusterMembership, crate::topology::ConsumeError>
    {
        crate::topology::TopologyConsumer::consume_bytes(bytes, uri_seed)
    }
}

fn parse_query_json(s: &str) -> Result<QueryResult> {
    let parsed: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| ClientError::new(ErrorCode::QueryError, format!("bad server JSON: {e}")))?;
    let statement = parsed
        .get("statement")
        .and_then(|v| v.as_str())
        .unwrap_or("select")
        .to_string();
    let affected = parsed
        .get("affected")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;
    let columns = parsed
        .get("columns")
        .and_then(|v| v.as_array())
        .map(|cols| {
            cols.iter()
                .filter_map(|col| col.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = parsed
        .get("rows")
        .or_else(|| parsed.get("records"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.iter().map(parse_row_value).collect())
        .unwrap_or_default();
    Ok(QueryResult {
        statement,
        affected,
        columns,
        rows,
    })
}

fn parse_row_value(value: &serde_json::Value) -> Vec<(String, ValueOut)> {
    value
        .as_object()
        .map(|row| {
            row.iter()
                .map(|(key, value)| (key.clone(), parse_scalar(value)))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_scalar(value: &serde_json::Value) -> ValueOut {
    match value {
        serde_json::Value::Null => ValueOut::Null,
        serde_json::Value::Bool(b) => ValueOut::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ValueOut::Integer(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 {
                    ValueOut::Integer(f as i64)
                } else {
                    ValueOut::Float(f)
                }
            } else {
                ValueOut::String(n.to_string())
            }
        }
        serde_json::Value::String(s) => ValueOut::String(s.clone()),
        other => ValueOut::String(other.to_string()),
    }
}

fn params_to_grpc_values(params: &[ParamValue]) -> Vec<reddb_grpc_proto::QueryValue> {
    use reddb_grpc_proto::query_value::Kind;
    use reddb_grpc_proto::{QueryNull, QueryValue, QueryVector};

    params
        .iter()
        .cloned()
        .map(|value| {
            let kind = match value {
                ParamValue::Null => Kind::NullValue(QueryNull {}),
                ParamValue::Bool(value) => Kind::BoolValue(value),
                ParamValue::Int(value) => Kind::IntValue(value),
                ParamValue::Float(value) => Kind::FloatValue(value),
                ParamValue::Text(value) => Kind::TextValue(value),
                ParamValue::Bytes(value) => Kind::BytesValue(value),
                ParamValue::Vector(values) => Kind::VectorValue(QueryVector { values }),
                ParamValue::Json(value) => Kind::JsonValue(value.to_json_string()),
                ParamValue::Timestamp(value) => Kind::TimestampValue(value),
                ParamValue::Uuid(value) => Kind::UuidValue(value.to_vec()),
            };
            QueryValue { kind: Some(kind) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_json_extracts_rows_and_columns() {
        let input = r#"{"statement":"select","affected":0,"columns":["id","name"],"rows":[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"}]}"#;
        let qr = parse_query_json(input).unwrap();
        assert_eq!(qr.statement, "select");
        assert_eq!(qr.affected, 0);
        assert_eq!(qr.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(qr.rows.len(), 2);
        assert_eq!(qr.rows[0][0].0, "id");
        assert!(matches!(qr.rows[0][0].1, ValueOut::Integer(1)));
        assert_eq!(qr.rows[1][1].0, "name");
        assert!(matches!(&qr.rows[1][1].1, ValueOut::String(s) if s == "Bob"));
    }

    #[test]
    fn parse_query_json_handles_empty_rows() {
        let input = r#"{"statement":"select","affected":0,"columns":[],"rows":[]}"#;
        let qr = parse_query_json(input).unwrap();
        assert!(qr.rows.is_empty());
        assert!(qr.columns.is_empty());
    }

    #[test]
    fn parse_query_json_tolerates_missing_fields() {
        // If server omits fields we fall back to empty defaults.
        let qr = parse_query_json("{}").unwrap();
        assert_eq!(qr.affected, 0);
        assert!(qr.rows.is_empty());
    }

    #[test]
    fn grpc_params_preserve_wire_value_variants() {
        use reddb_grpc_proto::query_value::Kind;

        let uuid = [0x11; 16];
        let params = vec![
            crate::params::Value::Null,
            crate::params::Value::Bool(true),
            crate::params::Value::Int(42),
            crate::params::Value::Float(1.5),
            crate::params::Value::Text("alice".into()),
            crate::params::Value::Bytes(vec![0, 1, 2]),
            crate::params::Value::Vector(vec![0.25, 0.5]),
            crate::params::Value::Json(crate::types::JsonValue::object([(
                "role",
                crate::types::JsonValue::string("admin"),
            )])),
            crate::params::Value::Timestamp(1_779_999_000),
            crate::params::Value::Uuid(uuid),
        ];

        let encoded = params_to_grpc_values(&params);
        assert_eq!(encoded.len(), 10);
        assert!(matches!(encoded[0].kind, Some(Kind::NullValue(_))));
        assert!(matches!(encoded[1].kind, Some(Kind::BoolValue(true))));
        assert!(matches!(encoded[2].kind, Some(Kind::IntValue(42))));
        assert!(matches!(encoded[3].kind, Some(Kind::FloatValue(1.5))));
        assert!(matches!(&encoded[4].kind, Some(Kind::TextValue(v)) if v == "alice"));
        assert!(matches!(&encoded[5].kind, Some(Kind::BytesValue(v)) if v == &[0, 1, 2]));
        assert!(
            matches!(&encoded[6].kind, Some(Kind::VectorValue(v)) if v.values == vec![0.25, 0.5])
        );
        assert!(
            matches!(&encoded[7].kind, Some(Kind::JsonValue(v)) if v == "{\"role\":\"admin\"}")
        );
        assert!(matches!(
            encoded[8].kind,
            Some(Kind::TimestampValue(1_779_999_000))
        ));
        assert!(matches!(&encoded[9].kind, Some(Kind::UuidValue(v)) if v == &uuid));
    }
}
