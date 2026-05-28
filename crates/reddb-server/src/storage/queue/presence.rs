//! Queue consumer presence — issue #742.
//!
//! Tracks consumer liveness as an explicit heartbeat contract that is
//! orthogonal to pending-delivery state. Red UI needs to render "is
//! anyone reading queue X / group Y right now?" without inferring it
//! from PEL entries: a worker can be alive on a quiet queue with zero
//! pending deliveries, and a worker with stuck PEL entries may itself
//! be dead. Presence is the answer to that question.
//!
//! Contract surface (process-local registry):
//!
//! - `heartbeat(queue, group, consumer, lease_count)` — record or
//!   refresh the last-seen timestamp for a `(queue, group, consumer)`
//!   triple. Called on every `QUEUE READ` (whether or not a message
//!   was returned) so an idle poller still counts as alive, and may
//!   be called explicitly by a `QUEUE HEARTBEAT` command in a follow-
//!   up slice.
//! - `snapshot(now_ns, ttl_ms)` — list every tracked consumer with
//!   the derived `last_seen_age_ms`, `lease_count`, and lifecycle
//!   flags (`active` / `stale` / `expired`). Stale means the consumer
//!   missed at least one heartbeat budget but is still tracked;
//!   expired means it crossed the prune horizon and is queued for
//!   removal on the next sweep.
//! - `count_active_by_group(now_ns, ttl_ms)` — per-`(queue, group)`
//!   active consumer count, the field the operator-facing metadata
//!   surfaces consume.
//! - `prune_expired(now_ns, ttl_ms)` — drop entries whose age exceeds
//!   `2 * ttl_ms` (the expiry horizon). Safe to call on every
//!   snapshot path or on a background timer.
//!
//! Aliveness model:
//!
//! - `age_ms <= ttl_ms`                      → **active**
//! - `ttl_ms < age_ms <= 2 * ttl_ms`         → **stale**  (one missed beat)
//! - `age_ms > 2 * ttl_ms`                   → **expired** (prune-eligible)
//!
//! Durability follow-up: this slice is the typed contract + the
//! metadata snapshot every consumer of presence (Red UI, `red.*`
//! virtual tables, drivers) talks to. Mirroring writes into
//! `red_queue_meta` rows so presence survives restart is the
//! immediately-next slice; the public surface here does not change
//! when that lands.

use std::collections::HashMap;
use std::sync::Mutex;

/// Default heartbeat budget — a consumer is considered active for
/// this long after its last beat. Operators can override per server
/// via the runtime config; the registry itself is agnostic and takes
/// the budget as an argument on every read path.
pub const DEFAULT_PRESENCE_TTL_MS: u64 = 30_000;

/// Lifecycle bucket derived from `last_seen_age_ms` vs the configured
/// `ttl_ms`. Snapshot consumers (Red UI, virtual tables) read this
/// flag and never re-derive the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    /// Last heartbeat within `ttl_ms`. Worker is alive.
    Active,
    /// Beyond `ttl_ms` but within `2 * ttl_ms`. Worker missed a beat;
    /// surface a warning in Red UI but keep the row visible.
    Stale,
    /// Beyond `2 * ttl_ms`. Worker is assumed dead; will be pruned on
    /// the next sweep but still appears in the snapshot until then so
    /// the UI can show "last seen 5 minutes ago" rather than a gap.
    Expired,
}

impl PresenceState {
    pub fn as_str(self) -> &'static str {
        match self {
            PresenceState::Active => "active",
            PresenceState::Stale => "stale",
            PresenceState::Expired => "expired",
        }
    }

    fn classify(age_ms: u64, ttl_ms: u64) -> Self {
        if age_ms <= ttl_ms {
            PresenceState::Active
        } else if age_ms <= ttl_ms.saturating_mul(2) {
            PresenceState::Stale
        } else {
            PresenceState::Expired
        }
    }
}

/// One row of presence state, returned by `snapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerPresence {
    pub queue: String,
    pub group: String,
    pub consumer: String,
    pub registered_at_ns: u64,
    pub last_heartbeat_ns: u64,
    /// `now_ns - last_heartbeat_ns`, milliseconds. Snapshotted at
    /// read time so the UI does not have to derive it.
    pub last_seen_age_ms: u64,
    /// Caller-reported number of in-flight (locked but unacked)
    /// messages for this consumer. Stored verbatim — the registry
    /// does not cross-check it against the live PEL because the
    /// presence contract is intentionally independent of pending
    /// delivery state.
    pub lease_count: u32,
    pub state: PresenceState,
}

#[derive(Debug, Clone)]
struct PresenceEntry {
    registered_at_ns: u64,
    last_heartbeat_ns: u64,
    lease_count: u32,
}

/// Composite key for the registry — kept private so callers can only
/// reach entries via the typed surface.
type PresenceKey = (String, String, String);

/// Process-local registry of consumer presence. Cheap mutex + small
/// hashmap is the right shape: writes are O(1), reads are a single
/// snapshot copy, and the cardinality is bounded by the operator's
/// worker fleet (typically dozens, not thousands).
#[derive(Debug, Default)]
pub struct ConsumerPresenceRegistry {
    entries: Mutex<HashMap<PresenceKey, PresenceEntry>>,
}

impl ConsumerPresenceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record or refresh a heartbeat. `now_ns` is taken from the
    /// caller so tests can drive a deterministic clock and so the
    /// runtime can reuse a wall-clock it already captured.
    pub fn heartbeat(
        &self,
        queue: &str,
        group: &str,
        consumer: &str,
        lease_count: u32,
        now_ns: u64,
    ) {
        let key = (queue.to_string(), group.to_string(), consumer.to_string());
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key)
            .and_modify(|e| {
                e.last_heartbeat_ns = now_ns;
                e.lease_count = lease_count;
            })
            .or_insert(PresenceEntry {
                registered_at_ns: now_ns,
                last_heartbeat_ns: now_ns,
                lease_count,
            });
    }

    /// Explicitly drop a consumer (e.g. on graceful shutdown). Returns
    /// whether an entry was actually removed.
    pub fn deregister(&self, queue: &str, group: &str, consumer: &str) -> bool {
        let key = (queue.to_string(), group.to_string(), consumer.to_string());
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(&key).is_some()
    }

    /// Full snapshot, deterministically ordered by `(queue, group,
    /// consumer)` so test assertions and Red UI tables both see a
    /// stable shape.
    pub fn snapshot(&self, now_ns: u64, ttl_ms: u64) -> Vec<ConsumerPresence> {
        let map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let mut rows: Vec<ConsumerPresence> = map
            .iter()
            .map(|((queue, group, consumer), entry)| {
                let age_ms = now_ns.saturating_sub(entry.last_heartbeat_ns) / 1_000_000;
                ConsumerPresence {
                    queue: queue.clone(),
                    group: group.clone(),
                    consumer: consumer.clone(),
                    registered_at_ns: entry.registered_at_ns,
                    last_heartbeat_ns: entry.last_heartbeat_ns,
                    last_seen_age_ms: age_ms,
                    lease_count: entry.lease_count,
                    state: PresenceState::classify(age_ms, ttl_ms),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            a.queue
                .cmp(&b.queue)
                .then_with(|| a.group.cmp(&b.group))
                .then_with(|| a.consumer.cmp(&b.consumer))
        });
        rows
    }

    /// Active-consumer count per `(queue, group)`. Only entries whose
    /// derived state is `Active` are counted — Red UI surfaces this
    /// as the "workers alive on this group right now" number, so
    /// stale/expired must not inflate it.
    pub fn count_active_by_group(
        &self,
        now_ns: u64,
        ttl_ms: u64,
    ) -> HashMap<(String, String), u32> {
        let mut by_group: HashMap<(String, String), u32> = HashMap::new();
        for row in self.snapshot(now_ns, ttl_ms) {
            if row.state == PresenceState::Active {
                *by_group.entry((row.queue, row.group)).or_insert(0) += 1;
            }
        }
        by_group
    }

    /// Drop entries whose `last_seen_age_ms` exceeds `2 * ttl_ms`.
    /// Returns the number of entries removed. Safe to call on any
    /// metadata-read path; not strictly required for correctness
    /// (snapshot already classifies them as `Expired`), but bounds
    /// memory after worker churn.
    pub fn prune_expired(&self, now_ns: u64, ttl_ms: u64) -> usize {
        let mut map = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let horizon_ns = ttl_ms.saturating_mul(2).saturating_mul(1_000_000);
        let before = map.len();
        map.retain(|_, entry| now_ns.saturating_sub(entry.last_heartbeat_ns) <= horizon_ns);
        before - map.len()
    }

    /// Total entry count (active + stale + expired). Mostly useful
    /// for tests and debug surfaces.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL_MS: u64 = 30_000;
    const MS_NS: u64 = 1_000_000;

    /// Acceptance: "Tests cover active consumer registration".
    #[test]
    fn first_heartbeat_registers_consumer_as_active() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        reg.heartbeat("orders", "workers", "w1", 0, t0);

        let snap = reg.snapshot(t0, TTL_MS);
        assert_eq!(snap.len(), 1);
        let row = &snap[0];
        assert_eq!(row.queue, "orders");
        assert_eq!(row.group, "workers");
        assert_eq!(row.consumer, "w1");
        assert_eq!(row.registered_at_ns, t0);
        assert_eq!(row.last_heartbeat_ns, t0);
        assert_eq!(row.last_seen_age_ms, 0);
        assert_eq!(row.lease_count, 0);
        assert_eq!(row.state, PresenceState::Active);
    }

    /// Acceptance: "Tests cover ... heartbeat update".
    #[test]
    fn heartbeat_refreshes_last_seen_but_preserves_registered_at() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        let t1 = t0 + 5_000 * MS_NS;
        reg.heartbeat("orders", "workers", "w1", 0, t0);
        reg.heartbeat("orders", "workers", "w1", 3, t1);

        let snap = reg.snapshot(t1, TTL_MS);
        assert_eq!(
            snap.len(),
            1,
            "heartbeat must update in place, not duplicate"
        );
        let row = &snap[0];
        assert_eq!(row.registered_at_ns, t0, "registered_at is sticky");
        assert_eq!(row.last_heartbeat_ns, t1);
        assert_eq!(row.last_seen_age_ms, 0);
        assert_eq!(row.lease_count, 3);
        assert_eq!(row.state, PresenceState::Active);
    }

    /// Acceptance: "Tests cover ... expiry".
    #[test]
    fn state_transitions_active_then_stale_then_expired() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        reg.heartbeat("orders", "workers", "w1", 0, t0);

        // Within TTL → Active.
        let in_ttl = t0 + (TTL_MS - 1) * MS_NS;
        assert_eq!(reg.snapshot(in_ttl, TTL_MS)[0].state, PresenceState::Active);

        // Between TTL and 2*TTL → Stale.
        let in_stale = t0 + (TTL_MS + 1) * MS_NS;
        let row = &reg.snapshot(in_stale, TTL_MS)[0];
        assert_eq!(row.state, PresenceState::Stale);
        assert_eq!(row.last_seen_age_ms, TTL_MS + 1);

        // Beyond 2*TTL → Expired.
        let in_expired = t0 + (TTL_MS * 2 + 1) * MS_NS;
        assert_eq!(
            reg.snapshot(in_expired, TTL_MS)[0].state,
            PresenceState::Expired
        );
    }

    #[test]
    fn prune_expired_removes_only_beyond_horizon() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        // active
        reg.heartbeat("q", "g", "alive", 0, t0);
        // stale (1.5 * TTL)
        reg.heartbeat("q", "g", "stale", 0, t0 - (TTL_MS + TTL_MS / 2) * MS_NS);
        // expired (3 * TTL)
        reg.heartbeat("q", "g", "expired", 0, t0 - TTL_MS * 3 * MS_NS);

        assert_eq!(reg.len(), 3);
        let pruned = reg.prune_expired(t0, TTL_MS);
        assert_eq!(pruned, 1, "only the >2*TTL entry is dropped");
        let names: Vec<_> = reg
            .snapshot(t0, TTL_MS)
            .into_iter()
            .map(|p| p.consumer)
            .collect();
        assert_eq!(names, vec!["alive".to_string(), "stale".to_string()]);
    }

    /// Acceptance: "Queue metadata includes active consumer count by
    /// queue and group" and "Tests cover ... queue/group visibility".
    #[test]
    fn count_active_by_group_segregates_queue_and_group() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;

        reg.heartbeat("orders", "workers", "w1", 0, t0);
        reg.heartbeat("orders", "workers", "w2", 0, t0);
        reg.heartbeat("orders", "audit", "a1", 0, t0);
        reg.heartbeat("billing", "workers", "b1", 0, t0);
        // stale — must not be counted as active
        reg.heartbeat("orders", "workers", "ghost", 0, t0 - (TTL_MS + 1) * MS_NS);

        let counts = reg.count_active_by_group(t0, TTL_MS);
        assert_eq!(counts[&("orders".into(), "workers".into())], 2);
        assert_eq!(counts[&("orders".into(), "audit".into())], 1);
        assert_eq!(counts[&("billing".into(), "workers".into())], 1);
        assert_eq!(counts.len(), 3, "stale ghost does not create a new bucket");
    }

    /// Acceptance: "The presence contract does not infer aliveness
    /// solely from pending deliveries."
    ///
    /// Encoded as a property: a consumer with `lease_count == 0` (no
    /// pending deliveries) that beats is still active; a consumer
    /// with `lease_count > 0` (PEL entries) that has not beat in
    /// `>TTL` is *not* active. Presence is heartbeat-driven, not
    /// PEL-driven.
    #[test]
    fn aliveness_is_heartbeat_driven_not_pending_driven() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;

        // Idle poller — no pending deliveries, fresh heartbeat.
        reg.heartbeat("q", "g", "idle_poller", 0, t0);
        // Worker that grabbed messages then died — still has PEL
        // leases, but its last heartbeat is ancient.
        reg.heartbeat("q", "g", "stuck_with_leases", 5, t0 - (TTL_MS * 3) * MS_NS);

        let snap = reg.snapshot(t0, TTL_MS);
        let by_consumer: HashMap<String, ConsumerPresence> =
            snap.into_iter().map(|p| (p.consumer.clone(), p)).collect();

        assert_eq!(
            by_consumer["idle_poller"].state,
            PresenceState::Active,
            "zero pending must not demote an actively-heartbeating consumer"
        );
        assert_eq!(by_consumer["idle_poller"].lease_count, 0);
        assert_eq!(
            by_consumer["stuck_with_leases"].state,
            PresenceState::Expired,
            "non-zero pending must not promote a consumer that stopped beating"
        );
        assert_eq!(by_consumer["stuck_with_leases"].lease_count, 5);

        let counts = reg.count_active_by_group(t0, TTL_MS);
        assert_eq!(
            counts.get(&("q".into(), "g".into())).copied().unwrap_or(0),
            1,
            "active count must reflect heartbeats, not pending deliveries"
        );
    }

    #[test]
    fn deregister_removes_consumer() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        reg.heartbeat("q", "g", "w1", 0, t0);
        reg.heartbeat("q", "g", "w2", 0, t0);
        assert!(reg.deregister("q", "g", "w1"));
        assert!(!reg.deregister("q", "g", "w1"), "second deregister no-ops");
        let names: Vec<_> = reg
            .snapshot(t0, TTL_MS)
            .into_iter()
            .map(|p| p.consumer)
            .collect();
        assert_eq!(names, vec!["w2".to_string()]);
    }

    #[test]
    fn snapshot_is_deterministically_ordered() {
        let reg = ConsumerPresenceRegistry::new();
        let t0 = 1_000_000_000_000_u64;
        // Insert in shuffled order.
        reg.heartbeat("zeta", "g", "c", 0, t0);
        reg.heartbeat("alpha", "z", "a", 0, t0);
        reg.heartbeat("alpha", "a", "z", 0, t0);
        reg.heartbeat("alpha", "a", "a", 0, t0);

        let snap = reg.snapshot(t0, TTL_MS);
        let shape: Vec<_> = snap
            .into_iter()
            .map(|p| (p.queue, p.group, p.consumer))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("alpha".into(), "a".into(), "a".into()),
                ("alpha".into(), "a".into(), "z".into()),
                ("alpha".into(), "z".into(), "a".into()),
                ("zeta".into(), "g".into(), "c".into()),
            ]
        );
    }
}
