//! #1274 — synchronous moderation gate + quarantine + tombstone, end-to-end
//! (PRD #1267, ADR 0057).
//!
//! A collection that declares a `MODERATE (... sync = true)` policy over
//! declared text fields gets:
//!   * a **synchronous pre-commit gate**: a reject fails the write (no row
//!     persists); a provider-down outcome either quarantines the row
//!     (fail-open default) or blocks the write (fail-closed opt-in);
//!   * **read-path hiding**: quarantine-pending and rejected-tombstone rows
//!     are excluded from normal SELECT/scan reads;
//!   * **async re-moderation** over the CDC enrichment lane: a quarantined
//!     row that re-moderates clean becomes visible, and one that
//!     re-moderates to a reject is tombstoned (hidden, retained for audit)
//!     or hard-deleted when the policy opts in.
//!
//! These tests install a deterministic mock moderation provider (no model
//! download, no network) whose verdict is a pure function of the screened
//! text plus a process-global "provider down" switch the test flips.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use reddb_server::runtime::ai::cdc_enrichment::{CdcEnrichmentConsumer, EnrichmentKind};
use reddb_server::runtime::ai::moderation::{
    install_local_moderation_backend, LocalModerationBackend, ModerationOutcome, ModerationRequest,
};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Sentinel substring: any screened text containing it is rejected by the
/// mock provider. Lets a single backend cover allow and reject in one test.
const TOXIC_MARKER: &str = "TOXIC";

/// Process-global switch: while true the mock reports the provider as down
/// so the degraded-mode (quarantine / fail-closed) paths are exercised.
static PROVIDER_DOWN: AtomicBool = AtomicBool::new(false);

/// Deterministic mock moderation provider. Rejects text containing
/// [`TOXIC_MARKER`], reports the provider down while [`PROVIDER_DOWN`] is
/// set, and otherwise allows.
struct MockModerationProvider;

impl LocalModerationBackend for MockModerationProvider {
    fn moderate(
        &self,
        request: &ModerationRequest,
    ) -> reddb_server::RedDBResult<ModerationOutcome> {
        if PROVIDER_DOWN.load(Ordering::SeqCst) {
            return Ok(ModerationOutcome::ProviderDown {
                reason: "mock provider down".to_string(),
            });
        }
        if request.text.contains(TOXIC_MARKER) {
            return Ok(ModerationOutcome::Reject {
                categories: vec!["harassment".to_string()],
            });
        }
        Ok(ModerationOutcome::Allow)
    }
}

/// The mock backend and [`PROVIDER_DOWN`] are process-global, so the tests
/// in this file (run in parallel threads of one binary) must not interleave
/// their provider-down toggling. Each test holds this guard for its whole
/// body, serialising them.
fn test_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Acquire the serialising guard, reset the provider-down switch, and
/// install the mock backend. The returned guard must be held for the whole
/// test body.
#[must_use]
fn install_mock() -> MutexGuard<'static, ()> {
    let guard = test_guard();
    PROVIDER_DOWN.store(false, Ordering::SeqCst);
    install_local_moderation_backend(Arc::new(MockModerationProvider));
    guard
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  err: {e}"))
        .result
        .records
        .len()
}

fn create_moderated_table(rt: &RedDBRuntime, name: &str, extra: &str) {
    rt.execute_query(&format!(
        "CREATE TABLE {name} (id INT, body TEXT) \
         WITH (MODERATE (fields = ('body'), provider = 'local', \
         model = 'mock-moderation', sync = true{extra}))"
    ))
    .unwrap_or_else(|e| panic!("create {name}: {e}"));
}

/// Acceptance: a reject fails the write — the row never persists (normal
/// path, provider up).
#[test]
fn reject_blocks_write() {
    let _guard = install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    create_moderated_table(&rt, "posts", "");

    // Clean content commits and is visible.
    rt.execute_query("INSERT INTO posts (id, body) VALUES (1, 'a friendly hello')")
        .expect("clean insert commits");
    assert_eq!(row_count(&rt, "SELECT * FROM posts"), 1);

    // Toxic content is rejected at the synchronous gate: the write errors
    // and the row never persists.
    let err = rt
        .execute_query(&format!(
            "INSERT INTO posts (id, body) VALUES (2, 'this is {TOXIC_MARKER} content')"
        ))
        .expect_err("toxic insert must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("moderation"),
        "reject error should mention moderation: {err}"
    );

    // Still exactly one row — the rejected write left nothing behind.
    assert_eq!(
        row_count(&rt, "SELECT * FROM posts"),
        1,
        "rejected row must not persist"
    );
}

/// Acceptance: provider-down default = fail-open + quarantine. The row
/// commits but is excluded from all normal reads, then is re-moderated
/// asynchronously via the CDC enrichment consumer; a clean re-moderation
/// makes it visible.
#[test]
fn provider_down_quarantine_then_clear_on_remoderation() {
    let _guard = install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    create_moderated_table(&rt, "msgs", ""); // degraded defaults to open

    // Provider is down at write time → fail-open quarantine.
    PROVIDER_DOWN.store(true, Ordering::SeqCst);
    rt.execute_query("INSERT INTO msgs (id, body) VALUES (1, 'benign while provider down')")
        .expect("quarantined insert still commits");

    // Excluded from all normal reads while quarantine-pending.
    assert_eq!(
        row_count(&rt, "SELECT * FROM msgs"),
        0,
        "quarantine-pending row must be hidden from normal reads"
    );

    // Provider recovers; CDC re-moderation runs and clears the quarantine.
    PROVIDER_DOWN.store(false, Ordering::SeqCst);
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert!(stats.ingested >= 1, "the quarantined row must be ingested");
    assert!(stats.attached >= 1, "re-moderation must complete");
    assert_eq!(consumer.pending_len(), 0);

    // Re-moderated clean → now visible.
    assert_eq!(
        row_count(&rt, "SELECT * FROM msgs"),
        1,
        "cleared row must surface after a clean re-moderation"
    );

    // Re-ticking must not re-enqueue the now-cleared row (the clear write-
    // back touches only the reserved marker, never a declared field).
    let again = consumer.tick(&rt, 2_000).expect("second tick");
    assert_eq!(again.ingested, 0, "cleared row must not re-enqueue");
}

/// Acceptance: a quarantined row that re-moderates to reject is tombstoned
/// + hidden by default (retained for audit/appeal).
#[test]
fn quarantine_then_remoderation_reject_tombstones() {
    let _guard = install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    create_moderated_table(&rt, "notes", ""); // degraded=open, hard_delete=false

    // Toxic content, but provider is down → fail-open quarantine (the gate
    // could not screen it yet).
    PROVIDER_DOWN.store(true, Ordering::SeqCst);
    rt.execute_query(&format!(
        "INSERT INTO notes (id, body) VALUES (1, 'sneaky {TOXIC_MARKER} note')"
    ))
    .expect("quarantined insert commits");
    assert_eq!(
        row_count(&rt, "SELECT * FROM notes"),
        0,
        "quarantine-pending row hidden"
    );

    // Provider recovers; re-moderation now sees the toxic content and
    // rejects → the row is tombstoned (hidden, retained).
    PROVIDER_DOWN.store(false, Ordering::SeqCst);
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert!(stats.ingested >= 1);
    assert!(stats.attached >= 1, "re-moderation must complete");

    // Rejected-tombstone row stays hidden from normal reads.
    assert_eq!(
        row_count(&rt, "SELECT * FROM notes"),
        0,
        "rejected-tombstone row must remain hidden"
    );
    // Re-ticking must not re-enqueue the tombstoned row.
    let again = consumer.tick(&rt, 2_000).expect("second tick");
    assert_eq!(again.ingested, 0, "tombstoned row must not re-enqueue");
}

/// Acceptance: per-collection opt-in hard-delete. A quarantined row that
/// re-moderates to reject under `hard_delete = true` is removed entirely.
#[test]
fn quarantine_reject_hard_delete_removes_row() {
    let _guard = install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    create_moderated_table(&rt, "ephemeral", ", hard_delete = true");

    PROVIDER_DOWN.store(true, Ordering::SeqCst);
    rt.execute_query(&format!(
        "INSERT INTO ephemeral (id, body) VALUES (1, 'bad {TOXIC_MARKER} stuff')"
    ))
    .expect("quarantined insert commits");

    PROVIDER_DOWN.store(false, Ordering::SeqCst);
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert!(stats.attached >= 1, "re-moderation must complete");

    // Hard-deleted → hidden, and a second tick finds nothing to do.
    assert_eq!(row_count(&rt, "SELECT * FROM ephemeral"), 0);
    let again = consumer.tick(&rt, 2_000).expect("second tick");
    assert_eq!(again.ingested, 0);
}

/// Acceptance: per-collection opt-in fail-closed. Provider-down blocks the
/// write instead of quarantining it.
#[test]
fn fail_closed_blocks_write_when_provider_down() {
    let _guard = install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    create_moderated_table(&rt, "strict", ", degraded = closed");

    PROVIDER_DOWN.store(true, Ordering::SeqCst);
    let err = rt
        .execute_query("INSERT INTO strict (id, body) VALUES (1, 'anything at all')")
        .expect_err("fail-closed must block the write when provider is down");
    assert!(
        err.to_string().to_lowercase().contains("moderation")
            || err.to_string().to_lowercase().contains("blocked"),
        "fail-closed error should explain the block: {err}"
    );

    // Nothing persisted, and nothing to re-moderate.
    PROVIDER_DOWN.store(false, Ordering::SeqCst);
    assert_eq!(
        row_count(&rt, "SELECT * FROM strict"),
        0,
        "fail-closed write must not persist"
    );
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert_eq!(stats.ingested, 0, "no quarantined row exists to ingest");

    // Sanity: with the provider up, the same write commits and is visible.
    rt.execute_query("INSERT INTO strict (id, body) VALUES (2, 'now it is fine')")
        .expect("provider-up write commits");
    assert_eq!(row_count(&rt, "SELECT * FROM strict"), 1);
}

/// The EnrichmentKind::Moderate variant is what the CDC lane queues for
/// quarantined rows; guard that the public surface exposes it.
#[test]
fn moderate_enrichment_kind_is_public() {
    let kind = EnrichmentKind::Moderate;
    assert_eq!(kind, EnrichmentKind::Moderate);
}
