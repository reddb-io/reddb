//! Durable stream primitive (issue #721, PRD #718).
//!
//! Tenant-scoped, append-only event log with monotonic per-consumer
//! offsets. Streams are the third leg of ADR 0028's split — queues
//! own per-message delivery state (ACK/NACK/DLQ), ephemeral
//! notifications own no state at all, and streams own an immutable
//! ordered log plus a small per-consumer offset bookkeeping table.
//! Reading a stream never creates pending delivery state and never
//! requires ACK or NACK; advancing the saved offset is the only
//! "I'm done with this prefix" signal.
//!
//! ## Contract surface
//!
//! - [`StreamRegistry::create_stream`] — declare a new stream with a
//!   retention contract. The stream becomes discoverable via
//!   [`StreamRegistry::list_streams`].
//! - [`StreamRegistry::append`] / [`StreamRegistry::append_authorized`]
//!   — append an event payload with an optional stream-identity key.
//!   Returns the assigned offset (`u64`, sequence within the stream).
//! - [`StreamRegistry::read_since`] /
//!   [`StreamRegistry::read_since_authorized`] — read up to `limit`
//!   events with offset `>= from`. Read does NOT consume, lease, or
//!   leave pending state behind, and does NOT advance the consumer's
//!   saved offset — that is the caller's explicit responsibility via
//!   `save_offset`.
//! - [`StreamRegistry::save_offset`] /
//!   [`StreamRegistry::get_offset`] — persist a consumer's progress.
//!   Saving is **monotonic**: a smaller or equal offset is dropped
//!   silently and the previously-saved value is returned. This makes
//!   the operation safe to retry on duplicate or stale acks without
//!   rewinding a consumer past events it already finished.
//!
//! ## Retention contract (first cut)
//!
//! Each stream carries a [`StreamRetention`] describing how the
//! engine prunes old events. The first cut supports two independent
//! caps that compose by AND (the stricter wins):
//!
//! * `max_events: Option<usize>` — drop the oldest events so the log
//!   never exceeds N entries.
//! * `max_age_ms: Option<u64>` — drop events older than `now -
//!   max_age_ms`.
//!
//! Retention is applied at append time. A retention pass never
//! rewrites the offset of surviving events — offsets remain sparse
//! once the head moves forward. Consumers whose saved offset has
//! fallen below the current head simply skip the truncated prefix
//! the next time they call `read_since`; the engine does not raise
//! an error for "consumer lagged past retention". Operators who care
//! about that condition can compare `get_offset(consumer)` against
//! the descriptor's `head_offset` themselves.
//!
//! ## Authorization model
//!
//! Mirrors the [`crate::notifications`] pattern: the registry never
//! consults policies directly. Transports evaluate the `stream`
//! action (and `stream:cross-tenant` for cross-tenant addressing)
//! against the principal's effective policies and pass the resulting
//! `has_cross_tenant_cap: bool` into the `_authorized` entry points.
//! Same-scope operations succeed without the extra capability;
//! everything else returns [`StreamError::CrossTenantDenied`] with
//! the principal / target / stream triple preserved for audit.
//!
//! ## CDC compatibility
//!
//! [`StreamEvent`] carries `key` and `payload` as opaque UTF-8
//! strings, plus the engine-assigned `offset` and `appended_at_ms`.
//! That shape is intentionally the standard change-data-capture log
//! shape: a later materialized-CDC slice can populate the same event
//! type from a table's mutation tail, with `key` becoming the row
//! primary key and `payload` the row JSON. Nothing in this module
//! commits the engine to a specific CDC strategy — the contract is
//! deliberately open on that axis.
//!
//! ## Durability
//!
//! The first slice of the primitive is an in-process append-only
//! log. The registry is `Send + Sync` and intended to live behind
//! an `Arc` on the runtime; persistence to disk-backed storage is a
//! follow-up slice tracked under the same PRD (#718) — it does not
//! change the public contract above, only where the bytes live.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

/// Scope of a stream — tenant-isolated by default. Mirrors
/// [`crate::notifications::NotificationScope`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StreamScope {
    /// Tenant-scoped stream — invisible to other tenants.
    Tenant(String),
    /// Cross-tenant / platform-global namespace.
    Global,
}

impl StreamScope {
    /// Construct a scope from a principal's tenant binding.
    ///
    /// `Some("acme")` → `Tenant("acme")`; `None` → `Global`
    /// (platform tenant). Same mapping as
    /// [`crate::notifications::NotificationScope::from_principal_tenant`]
    /// so a future transport can reuse the resolver.
    pub fn from_principal_tenant(tenant: Option<&str>) -> Self {
        match tenant {
            Some(t) => StreamScope::Tenant(t.to_string()),
            None => StreamScope::Global,
        }
    }

    /// Stable string identifier used in audit events.
    pub fn label(&self) -> String {
        match self {
            StreamScope::Tenant(t) => format!("tenant:{t}"),
            StreamScope::Global => "global".to_string(),
        }
    }
}

/// Retention contract for a single stream. Both fields are
/// independently optional; pass `StreamRetention::default()` for an
/// unbounded stream (no retention pruning).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamRetention {
    /// Maximum number of events retained. Oldest events past the cap
    /// are dropped on append. `None` means unbounded.
    pub max_events: Option<usize>,
    /// Maximum age in milliseconds. Events older than `now -
    /// max_age_ms` are dropped on append. `None` means unbounded.
    pub max_age_ms: Option<u64>,
}

/// A single event in the stream log. `offset` is engine-assigned
/// and monotonically increasing within `(scope, stream)`; retention
/// pruning may advance the head past low offsets but never reuses
/// or rewrites them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEvent {
    pub scope: StreamScope,
    pub stream: String,
    /// Optional stream-identity key. Carries the caller's
    /// partition / row-key hint; the engine does not interpret it
    /// in this slice. CDC-materialization slices may use it as the
    /// source row's primary key.
    pub key: Option<String>,
    /// Opaque UTF-8 payload — typically a JSON document. The engine
    /// does not parse or validate it.
    pub payload: String,
    /// Engine-assigned monotonic sequence number within
    /// `(scope, stream)`. The first event has offset `1`; offset
    /// `0` is reserved as the "no progress yet" sentinel returned
    /// by [`StreamRegistry::get_offset`] when a consumer has never
    /// saved.
    pub offset: u64,
    pub appended_at_ms: u128,
}

/// Public-facing descriptor for stream discovery — what an
/// introspection surface (e.g. a future `red.streams` virtual
/// table) would emit per stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDescriptor {
    pub scope: StreamScope,
    pub name: String,
    pub retention: StreamRetention,
    /// Offset of the oldest event still retained, or `0` if the
    /// stream is empty.
    pub head_offset: u64,
    /// Offset of the most recent event, or `0` if the stream is
    /// empty. The next append will receive `tail_offset + 1`.
    pub tail_offset: u64,
    pub event_count: usize,
}

/// Errors surfaced by the stream registry.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamError {
    /// `create_stream` was called for a `(scope, name)` pair that
    /// already has a stream.
    AlreadyExists { scope: StreamScope, name: String },
    /// An op targeted a stream that has not been created.
    NotFound { scope: StreamScope, name: String },
    /// The principal tried to address a stream outside their own
    /// tenant without the `stream:cross-tenant` capability.
    CrossTenantDenied {
        principal_tenant: Option<String>,
        target: StreamScope,
        stream: String,
    },
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::AlreadyExists { scope, name } => {
                write!(f, "stream: `{}/{}` already exists", scope.label(), name)
            }
            StreamError::NotFound { scope, name } => {
                write!(f, "stream: `{}/{}` not found", scope.label(), name)
            }
            StreamError::CrossTenantDenied {
                principal_tenant,
                target,
                stream,
            } => {
                let from = principal_tenant.as_deref().unwrap_or("<platform>");
                write!(
                    f,
                    "stream: principal in tenant `{}` is not allowed to address `{}` stream `{}` without the `stream:cross-tenant` capability",
                    from,
                    target.label(),
                    stream
                )
            }
        }
    }
}

impl std::error::Error for StreamError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StreamKey {
    scope: StreamScope,
    name: String,
}

#[derive(Debug)]
struct DurableStream {
    retention: StreamRetention,
    /// Append-only event log. Sorted by offset ascending.
    /// Retention drops from the front so `events[0].offset` is the
    /// current head.
    events: Vec<StreamEvent>,
    /// Next offset to assign. Starts at `1`; never decreases.
    next_offset: u64,
    /// Per-consumer saved offset. Always monotonic — see
    /// [`StreamRegistry::save_offset`].
    consumer_offsets: HashMap<String, u64>,
}

impl DurableStream {
    fn new(retention: StreamRetention) -> Self {
        Self {
            retention,
            events: Vec::new(),
            next_offset: 1,
            consumer_offsets: HashMap::new(),
        }
    }

    fn descriptor(&self, scope: StreamScope, name: String) -> StreamDescriptor {
        let head_offset = self.events.first().map(|e| e.offset).unwrap_or(0);
        let tail_offset = self.events.last().map(|e| e.offset).unwrap_or(0);
        StreamDescriptor {
            scope,
            name,
            retention: self.retention.clone(),
            head_offset,
            tail_offset,
            event_count: self.events.len(),
        }
    }

    fn apply_retention(&mut self, now_ms: u128) {
        if let Some(max_events) = self.retention.max_events {
            while self.events.len() > max_events {
                self.events.remove(0);
            }
        }
        if let Some(max_age_ms) = self.retention.max_age_ms {
            let cutoff = now_ms.saturating_sub(max_age_ms as u128);
            while let Some(first) = self.events.first() {
                if first.appended_at_ms < cutoff {
                    self.events.remove(0);
                } else {
                    break;
                }
            }
        }
    }
}

/// In-memory registry of durable streams.
#[derive(Default, Clone)]
pub struct StreamRegistry {
    inner: Arc<Mutex<HashMap<StreamKey, DurableStream>>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a new stream. Returns
    /// [`StreamError::AlreadyExists`] if `(scope, name)` is already
    /// registered. The new stream is immediately discoverable via
    /// [`Self::list_streams`].
    pub fn create_stream(
        &self,
        scope: StreamScope,
        name: impl Into<String>,
        retention: StreamRetention,
    ) -> Result<(), StreamError> {
        let name = name.into();
        let key = StreamKey {
            scope: scope.clone(),
            name: name.clone(),
        };
        let mut guard = self.inner.lock();
        if guard.contains_key(&key) {
            return Err(StreamError::AlreadyExists { scope, name });
        }
        guard.insert(key, DurableStream::new(retention));
        Ok(())
    }

    /// Whether `(scope, name)` is registered.
    pub fn exists(&self, scope: &StreamScope, name: &str) -> bool {
        let key = StreamKey {
            scope: scope.clone(),
            name: name.to_string(),
        };
        self.inner.lock().contains_key(&key)
    }

    /// Snapshot every stream in `scope`. Used by introspection
    /// surfaces (e.g. a future `red.streams` virtual table). Order
    /// is unspecified; callers that need stable order should sort
    /// on `name`.
    pub fn list_streams(&self, scope: &StreamScope) -> Vec<StreamDescriptor> {
        let guard = self.inner.lock();
        guard
            .iter()
            .filter(|(k, _)| &k.scope == scope)
            .map(|(k, s)| s.descriptor(k.scope.clone(), k.name.clone()))
            .collect()
    }

    /// Describe a single stream, or `None` if not registered.
    pub fn describe(&self, scope: &StreamScope, name: &str) -> Option<StreamDescriptor> {
        let key = StreamKey {
            scope: scope.clone(),
            name: name.to_string(),
        };
        let guard = self.inner.lock();
        guard.get(&key).map(|s| s.descriptor(key.scope, key.name))
    }

    /// Append an event. Returns the engine-assigned offset.
    /// Retention pruning runs after the append, so the new event
    /// is always retained even if it pushes the head past the cap
    /// — only older events are dropped.
    pub fn append(
        &self,
        scope: StreamScope,
        name: impl Into<String>,
        key: Option<String>,
        payload: impl Into<String>,
        now_ms: u128,
    ) -> Result<u64, StreamError> {
        let name = name.into();
        let lookup_key = StreamKey {
            scope: scope.clone(),
            name: name.clone(),
        };
        let mut guard = self.inner.lock();
        let stream = guard
            .get_mut(&lookup_key)
            .ok_or_else(|| StreamError::NotFound {
                scope: scope.clone(),
                name: name.clone(),
            })?;
        let offset = stream.next_offset;
        stream.next_offset += 1;
        stream.events.push(StreamEvent {
            scope,
            stream: name,
            key,
            payload: payload.into(),
            offset,
            appended_at_ms: now_ms,
        });
        stream.apply_retention(now_ms);
        Ok(offset)
    }

    /// Authorization-gated [`Self::append`].
    pub fn append_authorized(
        &self,
        principal_tenant: Option<&str>,
        target: StreamScope,
        name: impl Into<String>,
        key: Option<String>,
        payload: impl Into<String>,
        has_cross_tenant_cap: bool,
        now_ms: u128,
    ) -> Result<u64, StreamError> {
        let name = name.into();
        Self::authorize(principal_tenant, &target, &name, has_cross_tenant_cap)?;
        self.append(target, name, key, payload, now_ms)
    }

    /// Read up to `limit` events with offset `>= from`. Pure read —
    /// does not create pending delivery state, does not advance
    /// any consumer's saved offset, and does not require ACK/NACK.
    /// If `from` is below the current head (because retention has
    /// pruned older events), the returned slice simply starts at
    /// the head with no error.
    pub fn read_since(
        &self,
        scope: &StreamScope,
        name: &str,
        from: u64,
        limit: usize,
    ) -> Result<Vec<StreamEvent>, StreamError> {
        let key = StreamKey {
            scope: scope.clone(),
            name: name.to_string(),
        };
        let guard = self.inner.lock();
        let stream = guard.get(&key).ok_or_else(|| StreamError::NotFound {
            scope: scope.clone(),
            name: name.to_string(),
        })?;
        Ok(stream
            .events
            .iter()
            .filter(|e| e.offset >= from)
            .take(limit)
            .cloned()
            .collect())
    }

    /// Authorization-gated [`Self::read_since`].
    pub fn read_since_authorized(
        &self,
        principal_tenant: Option<&str>,
        target: StreamScope,
        name: impl Into<String>,
        from: u64,
        limit: usize,
        has_cross_tenant_cap: bool,
    ) -> Result<Vec<StreamEvent>, StreamError> {
        let name = name.into();
        Self::authorize(principal_tenant, &target, &name, has_cross_tenant_cap)?;
        self.read_since(&target, &name, from, limit)
    }

    /// Persist a consumer's offset on `(scope, name)`. Monotonic:
    /// if `offset` is less than or equal to the currently saved
    /// value, the save is a no-op and the existing value is
    /// returned. Otherwise the new value is stored and returned.
    /// This makes the operation safe to retry on duplicate or
    /// stale "I'm done with offset N" notifications — a consumer
    /// can never rewind past events it already finished.
    pub fn save_offset(
        &self,
        scope: &StreamScope,
        name: &str,
        consumer: &str,
        offset: u64,
    ) -> Result<u64, StreamError> {
        let key = StreamKey {
            scope: scope.clone(),
            name: name.to_string(),
        };
        let mut guard = self.inner.lock();
        let stream = guard.get_mut(&key).ok_or_else(|| StreamError::NotFound {
            scope: scope.clone(),
            name: name.to_string(),
        })?;
        let entry = stream
            .consumer_offsets
            .entry(consumer.to_string())
            .or_insert(0);
        if offset > *entry {
            *entry = offset;
        }
        Ok(*entry)
    }

    /// Retrieve a consumer's saved offset for `(scope, name)`.
    /// Returns `0` for consumers that have never saved — `0` is
    /// the reserved "no progress yet" sentinel since the first
    /// real event is at offset `1`.
    pub fn get_offset(
        &self,
        scope: &StreamScope,
        name: &str,
        consumer: &str,
    ) -> Result<u64, StreamError> {
        let key = StreamKey {
            scope: scope.clone(),
            name: name.to_string(),
        };
        let guard = self.inner.lock();
        let stream = guard.get(&key).ok_or_else(|| StreamError::NotFound {
            scope: scope.clone(),
            name: name.to_string(),
        })?;
        Ok(stream.consumer_offsets.get(consumer).copied().unwrap_or(0))
    }

    fn authorize(
        principal_tenant: Option<&str>,
        target: &StreamScope,
        stream: &str,
        has_cross_tenant_cap: bool,
    ) -> Result<(), StreamError> {
        let same_scope = match (principal_tenant, target) {
            (Some(pt), StreamScope::Tenant(tt)) => pt == tt,
            // Platform principal (tenant=None) addressing Global is
            // same-scope and needs no extra cap, matching the
            // notifications precedent.
            (None, StreamScope::Global) => true,
            _ => false,
        };
        if same_scope || has_cross_tenant_cap {
            return Ok(());
        }
        Err(StreamError::CrossTenantDenied {
            principal_tenant: principal_tenant.map(str::to_string),
            target: target.clone(),
            stream: stream.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> StreamScope {
        StreamScope::Tenant(name.into())
    }

    #[test]
    fn create_then_discover_via_list() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        let listed = reg.list_streams(&t("acme"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "orders");
        assert_eq!(listed[0].event_count, 0);
        assert_eq!(listed[0].head_offset, 0);
        assert_eq!(listed[0].tail_offset, 0);
        assert!(reg.exists(&t("acme"), "orders"));
        assert!(reg.describe(&t("acme"), "orders").is_some());
    }

    #[test]
    fn duplicate_create_rejected() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        let err = reg
            .create_stream(t("acme"), "orders", StreamRetention::default())
            .expect_err("dup create must fail");
        assert!(matches!(err, StreamError::AlreadyExists { .. }));
    }

    #[test]
    fn append_assigns_monotonic_offsets() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        let o1 = reg.append(t("acme"), "orders", None, "a", 100).unwrap();
        let o2 = reg
            .append(t("acme"), "orders", Some("k".into()), "b", 101)
            .unwrap();
        let o3 = reg.append(t("acme"), "orders", None, "c", 102).unwrap();
        assert_eq!((o1, o2, o3), (1, 2, 3));
        let desc = reg.describe(&t("acme"), "orders").unwrap();
        assert_eq!(desc.head_offset, 1);
        assert_eq!(desc.tail_offset, 3);
        assert_eq!(desc.event_count, 3);
    }

    #[test]
    fn append_on_unknown_stream_errors() {
        let reg = StreamRegistry::new();
        let err = reg
            .append(t("acme"), "missing", None, "x", 0)
            .expect_err("append on unknown stream must error");
        assert!(matches!(err, StreamError::NotFound { .. }));
    }

    #[test]
    fn read_since_returns_events_from_offset() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        for (i, payload) in ["a", "b", "c", "d"].iter().enumerate() {
            reg.append(t("acme"), "orders", None, *payload, 100 + i as u128)
                .unwrap();
        }
        let from_start = reg.read_since(&t("acme"), "orders", 0, 100).unwrap();
        assert_eq!(from_start.len(), 4);
        assert_eq!(from_start[0].offset, 1);
        assert_eq!(from_start[3].payload, "d");

        let from_middle = reg.read_since(&t("acme"), "orders", 3, 100).unwrap();
        assert_eq!(from_middle.len(), 2);
        assert_eq!(from_middle[0].offset, 3);
        assert_eq!(from_middle[1].offset, 4);

        let bounded = reg.read_since(&t("acme"), "orders", 0, 2).unwrap();
        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[1].offset, 2);
    }

    #[test]
    fn read_does_not_advance_consumer_offset_no_pending_state() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        for i in 0..3 {
            reg.append(t("acme"), "orders", None, "x", i).unwrap();
        }
        // Read everything multiple times. No ACK/NACK in the API,
        // and get_offset stays at 0 — proves read leaves no pending
        // delivery state behind.
        for _ in 0..3 {
            let events = reg.read_since(&t("acme"), "orders", 0, 100).unwrap();
            assert_eq!(events.len(), 3);
        }
        assert_eq!(reg.get_offset(&t("acme"), "orders", "c1").unwrap(), 0);
    }

    #[test]
    fn save_offset_is_monotonic() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        for i in 0..5 {
            reg.append(t("acme"), "orders", None, "x", i).unwrap();
        }
        assert_eq!(reg.save_offset(&t("acme"), "orders", "c1", 3).unwrap(), 3);
        // Stale save (smaller) is a no-op.
        assert_eq!(
            reg.save_offset(&t("acme"), "orders", "c1", 1).unwrap(),
            3,
            "stale save must not rewind",
        );
        // Equal save is a no-op (idempotent retry).
        assert_eq!(reg.save_offset(&t("acme"), "orders", "c1", 3).unwrap(), 3,);
        // Advance forward.
        assert_eq!(reg.save_offset(&t("acme"), "orders", "c1", 5).unwrap(), 5,);
        assert_eq!(reg.get_offset(&t("acme"), "orders", "c1").unwrap(), 5);
    }

    #[test]
    fn get_offset_defaults_to_zero_for_new_consumer() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        assert_eq!(reg.get_offset(&t("acme"), "orders", "fresh").unwrap(), 0);
    }

    #[test]
    fn consumer_offsets_are_isolated_per_consumer() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        reg.append(t("acme"), "orders", None, "x", 0).unwrap();
        reg.save_offset(&t("acme"), "orders", "c1", 1).unwrap();
        assert_eq!(reg.get_offset(&t("acme"), "orders", "c1").unwrap(), 1);
        assert_eq!(reg.get_offset(&t("acme"), "orders", "c2").unwrap(), 0);
    }

    #[test]
    fn streams_are_tenant_isolated() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        reg.create_stream(t("globex"), "orders", StreamRetention::default())
            .unwrap();
        reg.append(t("acme"), "orders", None, "acme-only", 0)
            .unwrap();
        let globex_events = reg.read_since(&t("globex"), "orders", 0, 100).unwrap();
        assert!(
            globex_events.is_empty(),
            "globex must not see acme's events"
        );
        // Same name in different scopes resolves to different
        // streams — list filters by scope.
        assert_eq!(reg.list_streams(&t("acme")).len(), 1);
        assert_eq!(reg.list_streams(&t("globex")).len(), 1);
    }

    #[test]
    fn retention_max_events_drops_oldest() {
        let reg = StreamRegistry::new();
        reg.create_stream(
            t("acme"),
            "orders",
            StreamRetention {
                max_events: Some(3),
                max_age_ms: None,
            },
        )
        .unwrap();
        for i in 0..5 {
            reg.append(t("acme"), "orders", None, "x", 100 + i as u128)
                .unwrap();
        }
        let desc = reg.describe(&t("acme"), "orders").unwrap();
        // Newest 3 retained: offsets 3, 4, 5. Head moved to 3.
        assert_eq!(desc.event_count, 3);
        assert_eq!(desc.head_offset, 3);
        assert_eq!(desc.tail_offset, 5);
        let events = reg.read_since(&t("acme"), "orders", 0, 100).unwrap();
        assert_eq!(
            events.iter().map(|e| e.offset).collect::<Vec<_>>(),
            vec![3, 4, 5],
        );
    }

    #[test]
    fn retention_max_age_drops_old_events() {
        let reg = StreamRegistry::new();
        reg.create_stream(
            t("acme"),
            "orders",
            StreamRetention {
                max_events: None,
                max_age_ms: Some(1_000),
            },
        )
        .unwrap();
        reg.append(t("acme"), "orders", None, "old", 0).unwrap();
        reg.append(t("acme"), "orders", None, "old2", 500).unwrap();
        // This append's `now_ms` triggers a retention pass — events
        // with appended_at_ms < (10_000 - 1_000) = 9_000 are dropped.
        reg.append(t("acme"), "orders", None, "fresh", 10_000)
            .unwrap();
        let events = reg.read_since(&t("acme"), "orders", 0, 100).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload, "fresh");
        assert_eq!(events[0].offset, 3, "retention must not rewrite offsets");
    }

    #[test]
    fn consumer_lagged_past_retention_does_not_error() {
        let reg = StreamRegistry::new();
        reg.create_stream(
            t("acme"),
            "orders",
            StreamRetention {
                max_events: Some(2),
                max_age_ms: None,
            },
        )
        .unwrap();
        for i in 0..5 {
            reg.append(t("acme"), "orders", None, "x", i).unwrap();
        }
        // Consumer had saved offset 1, but retention has advanced
        // the head to 4. read_since must just return the current
        // window rather than erroring — operators detect lag by
        // comparing get_offset against descriptor.head_offset.
        let events = reg.read_since(&t("acme"), "orders", 2, 100).unwrap();
        assert_eq!(
            events.iter().map(|e| e.offset).collect::<Vec<_>>(),
            vec![4, 5],
        );
    }

    #[test]
    fn same_tenant_append_does_not_require_cross_tenant_cap() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "orders", StreamRetention::default())
            .unwrap();
        let offset = reg
            .append_authorized(Some("acme"), t("acme"), "orders", None, "x", false, 0)
            .expect("same-tenant append must succeed without cross-tenant cap");
        assert_eq!(offset, 1);

        reg.read_since_authorized(Some("acme"), t("acme"), "orders", 0, 100, false)
            .expect("same-tenant read must succeed without cross-tenant cap");
    }

    #[test]
    fn cross_tenant_append_denied_without_cap() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("globex"), "orders", StreamRetention::default())
            .unwrap();
        let err = reg
            .append_authorized(Some("acme"), t("globex"), "orders", None, "leak", false, 0)
            .expect_err("cross-tenant append must be denied without cap");
        match err {
            StreamError::CrossTenantDenied {
                principal_tenant,
                target,
                stream,
            } => {
                assert_eq!(principal_tenant.as_deref(), Some("acme"));
                assert_eq!(target, t("globex"));
                assert_eq!(stream, "orders");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn cross_tenant_read_denied_without_cap() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("globex"), "orders", StreamRetention::default())
            .unwrap();
        let err = reg
            .read_since_authorized(Some("acme"), t("globex"), "orders", 0, 100, false)
            .expect_err("cross-tenant read must be denied without cap");
        assert!(matches!(err, StreamError::CrossTenantDenied { .. }));
    }

    #[test]
    fn cross_tenant_append_allowed_with_cap() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("globex"), "orders", StreamRetention::default())
            .unwrap();
        let offset = reg
            .append_authorized(
                Some("acme"),
                t("globex"),
                "orders",
                None,
                "allowed",
                true,
                0,
            )
            .expect("append with cross-tenant cap must succeed");
        assert_eq!(offset, 1);
    }

    #[test]
    fn global_scope_requires_cross_tenant_cap_for_tenant_principal() {
        let reg = StreamRegistry::new();
        reg.create_stream(StreamScope::Global, "platform", StreamRetention::default())
            .unwrap();
        let err = reg
            .append_authorized(
                Some("acme"),
                StreamScope::Global,
                "platform",
                None,
                "leak",
                false,
                0,
            )
            .expect_err("tenant principal targeting Global must require cap");
        assert!(matches!(err, StreamError::CrossTenantDenied { .. }));

        // Platform principal (tenant=None) targeting Global is
        // same-scope and needs no extra cap.
        let offset = reg
            .append_authorized(None, StreamScope::Global, "platform", None, "ok", false, 0)
            .expect("platform principal targeting global is same-scope");
        assert_eq!(offset, 1);
    }

    #[test]
    fn from_principal_tenant_maps_correctly() {
        assert_eq!(
            StreamScope::from_principal_tenant(Some("acme")),
            StreamScope::Tenant("acme".into())
        );
        assert_eq!(
            StreamScope::from_principal_tenant(None),
            StreamScope::Global
        );
    }

    #[test]
    fn event_carries_optional_key_for_future_cdc() {
        let reg = StreamRegistry::new();
        reg.create_stream(t("acme"), "rows", StreamRetention::default())
            .unwrap();
        reg.append(t("acme"), "rows", Some("user:42".into()), "{}", 0)
            .unwrap();
        let events = reg.read_since(&t("acme"), "rows", 0, 100).unwrap();
        assert_eq!(events[0].key.as_deref(), Some("user:42"));
    }
}
