//! Issue #840 — auto-rollback of a deposed primary to the common point,
//! preserving the divergent tail (PRD #819, ADR 0030).
//!
//! Models a deposed primary rejoining after a failover while still
//! holding a *divergent tail* — writes above the point its log last
//! agreed with the new primary. The [`RollbackCoordinator`] recovers the
//! node to the common point (bounded by the commit watermark), preserves
//! the discarded tail to a rollback file, fires a loud operator event,
//! and rejoins the node as a replica. The test asserts the four
//! acceptance criteria:
//!
//! 1. recover to the common point + rejoin without operator surgery;
//! 2. the discarded tail is persisted to a rollback file *and* surfaced
//!    via a real `OperatorEvent` written to the tamper-evident audit log;
//! 3. nothing at or below the commit watermark is rolled back;
//! 4. the rollback boundary, tail preservation, and event emission hold.
//!
//! The full "induce a live failover with a divergent tail" end-to-end
//! exercise depends on the election (#834) and stale-term fencing (#835)
//! being live; those are informational dependencies for this slice, so
//! the cluster here is a deterministic in-memory model that drives the
//! recover-to-LSN mechanism exactly as the live wiring will.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::time::Duration;

use reddb::replication::failover::NodeRole;
use reddb::replication::rollback::{
    DivergentTail, RollbackCoordinator, RollbackError, RollbackEvent, RollbackRequest,
    RollbackTransport, TailRecord,
};
use reddb::runtime::audit_log::AuditLogger;
use reddb::telemetry::operator_event::OperatorEvent;

/// A modelled deposed-primary node. Its `timeline` is the ordered list of
/// LSNs currently materialised on the live store; recover-to-LSN truncates
/// it to the common point. A real [`AuditLogger`] receives the operator
/// event so the test proves the *loud* event wiring, not just a captured
/// struct.
struct DeposedPrimary {
    /// LSNs currently on the live timeline (sorted ascending).
    timeline: Vec<u64>,
    /// Term each LSN was written under (parallel to `timeline`).
    term_of: Vec<u64>,
    /// Directory rollback files are written to.
    rollback_dir: PathBuf,
    /// Real audit logger the operator event is emitted through.
    audit: AuditLogger,
    /// Live role of the node; flips to Replica on rejoin.
    role: Option<NodeRole>,
}

impl DeposedPrimary {
    fn new(timeline: Vec<(u64, u64)>, rollback_dir: PathBuf, audit: AuditLogger) -> Self {
        let term_of = timeline.iter().map(|(_, t)| *t).collect();
        let timeline = timeline.iter().map(|(lsn, _)| *lsn).collect();
        Self {
            timeline,
            term_of,
            rollback_dir,
            audit,
            role: None,
        }
    }

    fn frontier(&self) -> u64 {
        self.timeline.last().copied().unwrap_or(0)
    }
}

impl RollbackTransport for DeposedPrimary {
    fn read_divergent_tail(&mut self, from_exclusive: u64, to_inclusive: u64) -> Vec<TailRecord> {
        self.timeline
            .iter()
            .zip(self.term_of.iter())
            .filter(|(lsn, _)| **lsn > from_exclusive && **lsn <= to_inclusive)
            .map(|(lsn, term)| TailRecord::new(*lsn, *term, lsn.to_le_bytes().to_vec()))
            .collect()
    }

    fn persist_rollback_file(&mut self, tail: &DivergentTail) -> Result<String, String> {
        std::fs::create_dir_all(&self.rollback_dir).map_err(|e| e.to_string())?;
        let path = self.rollback_dir.join(format!(
            "rollback-lsn-{}-{}.rbk",
            tail.common_point_lsn, tail.to_lsn
        ));
        // Persist one line per discarded record so an operator can
        // reconcile them later.
        let mut body = format!(
            "# divergent tail discarded on rejoin: common_point={} to={}\n",
            tail.common_point_lsn, tail.to_lsn
        );
        for r in &tail.records {
            body.push_str(&format!(
                "lsn={} term={} bytes={}\n",
                r.lsn,
                r.term,
                r.payload.len()
            ));
        }
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn recover_to_lsn(&mut self, target_lsn: u64) -> Result<(), String> {
        // Drop every materialised version strictly above the target — the
        // live timeline now ends at the common point.
        let keep: Vec<usize> = self
            .timeline
            .iter()
            .enumerate()
            .filter(|(_, lsn)| **lsn <= target_lsn)
            .map(|(i, _)| i)
            .collect();
        self.timeline = keep.iter().map(|i| self.timeline[*i]).collect();
        self.term_of = keep.iter().map(|i| self.term_of[*i]).collect();
        Ok(())
    }

    fn emit_rollback_event(&mut self, event: RollbackEvent) {
        // Forward to the REAL operator event bus (tamper-evident audit
        // log first), exactly as the production transport will.
        OperatorEvent::DeposedPrimaryRollback {
            common_point_lsn: event.common_point_lsn,
            tail_to_lsn: event.tail_to_lsn,
            tail_lsns: event.tail_lsns,
            commit_watermark: event.commit_watermark,
            rollback_file: event.rollback_file,
            new_primary_addr: event.new_primary_addr,
            new_term: event.new_term,
        }
        .emit(&self.audit);
    }

    fn rejoin_as_replica(&mut self, primary_addr: &str, term: u64) {
        self.role = Some(NodeRole::Replica {
            primary_addr: primary_addr.to_string(),
            term,
        });
    }
}

fn make_audit() -> (support::TempDataDir, AuditLogger, PathBuf) {
    let dir = support::temp_data_dir("840");
    let path = dir.join(".audit.log");
    (dir, AuditLogger::with_path(path.clone()), path)
}

fn last_audit_line(path: &std::path::Path) -> reddb::json::Value {
    let body = std::fs::read_to_string(path).unwrap();
    let line = body.lines().last().expect("at least one audit line");
    reddb::json::from_str(line).expect("valid JSON")
}

#[test]
fn deposed_primary_auto_rolls_back_divergent_tail_and_rejoins() {
    let (_dir, audit, audit_path) = make_audit();
    let rollback_dir = audit_path.parent().unwrap().join("rollback");

    // The deposed primary's timeline: committed history up to 200 (term 7)
    // then a divergent tail 210/220/230 it wrote alone before being
    // deposed and never replicated to quorum.
    let mut node = DeposedPrimary::new(
        vec![(180, 7), (200, 7), (210, 7), (220, 7), (230, 7)],
        rollback_dir.clone(),
        audit,
    );
    assert_eq!(node.frontier(), 230);

    // The election (#834) produced common point 200, and the commit
    // watermark (#822) is 200 — everything at/below 200 reached quorum.
    let req = RollbackRequest {
        local_frontier: node.frontier(),
        common_point: 200,
        commit_watermark: 200,
        new_primary_addr: "http://node-b:50051".to_string(),
        new_term: 8,
    };

    let outcome = RollbackCoordinator::run(&req, &mut node).expect("auto-rollback succeeds");

    // (1) Recovered to the common point and rejoined as a replica — no
    // operator surgery.
    assert_eq!(outcome.recovered_to_lsn, 200);
    assert_eq!(
        node.role,
        Some(NodeRole::Replica {
            primary_addr: "http://node-b:50051".to_string(),
            term: 8,
        }),
    );

    // (3) Nothing at or below the watermark (200) was rolled back; the
    // divergent tail above it is gone.
    assert_eq!(
        node.timeline,
        vec![180, 200],
        "tail above 200 removed, history kept"
    );
    assert_eq!(node.frontier(), 200);
    assert_eq!(outcome.tail_lsns, 30);

    // (2) The discarded tail is persisted to a rollback file...
    let file = outcome.rollback_file.expect("rollback file written");
    let saved = std::fs::read_to_string(&file).expect("rollback file readable");
    assert!(saved.contains("lsn=210"));
    assert!(saved.contains("lsn=220"));
    assert!(saved.contains("lsn=230"));
    assert!(
        !saved.contains("lsn=200"),
        "committed LSN must not be in the rollback file"
    );

    // ...and surfaced via a real operator event on the tamper-evident
    // audit log.
    assert!(outcome.event_fired);
    assert!(
        node.audit.wait_idle(Duration::from_secs(2)),
        "audit drain timed out"
    );
    let ev = last_audit_line(&audit_path);
    assert_eq!(
        ev.get("action").and_then(|x| x.as_str()),
        Some("operator/deposed_primary_rollback"),
    );
    let detail = ev.get("detail").expect("event detail");
    assert_eq!(
        detail.get("common_point_lsn").and_then(|x| x.as_i64()),
        Some(200)
    );
    assert_eq!(detail.get("tail_lsns").and_then(|x| x.as_i64()), Some(30));
    assert_eq!(
        detail.get("commit_watermark").and_then(|x| x.as_i64()),
        Some(200)
    );
}

#[test]
fn rollback_refuses_to_cross_the_commit_watermark() {
    // A pathological common point BELOW the watermark would discard
    // committed data. The coordinator must refuse and change nothing —
    // the invariant "nothing at/below the watermark is ever rolled back"
    // is enforced even against a bad election result.
    let (_dir, audit, audit_path) = make_audit();
    let rollback_dir = audit_path.parent().unwrap().join("rollback");
    let mut node = DeposedPrimary::new(vec![(150, 7), (200, 7), (250, 7)], rollback_dir, audit);

    let req = RollbackRequest {
        local_frontier: 250,
        common_point: 150, // below the watermark!
        commit_watermark: 200,
        new_primary_addr: "http://node-b:50051".to_string(),
        new_term: 8,
    };

    let err = RollbackCoordinator::run(&req, &mut node).expect_err("must refuse");
    assert!(matches!(err, RollbackError::WatermarkViolation { .. }));

    // Nothing changed: timeline intact, no rejoin, no event.
    assert_eq!(node.timeline, vec![150, 200, 250]);
    assert!(node.role.is_none());
    assert!(
        !std::path::Path::new(&audit_path).exists() || {
            // Audit file may exist but must carry no rollback event.
            let body = std::fs::read_to_string(&audit_path).unwrap_or_default();
            !body.contains("deposed_primary_rollback")
        }
    );
}

#[test]
fn caught_up_ex_primary_rejoins_without_a_rollback() {
    // An ex-primary whose frontier equals the common point has no
    // divergent tail: it rejoins cleanly, with no rollback file and no
    // operator event.
    let (_dir, audit, audit_path) = make_audit();
    let rollback_dir = audit_path.parent().unwrap().join("rollback");
    let mut node = DeposedPrimary::new(vec![(180, 7), (200, 7)], rollback_dir, audit);

    let req = RollbackRequest {
        local_frontier: 200,
        common_point: 200,
        commit_watermark: 200,
        new_primary_addr: "http://node-b:50051".to_string(),
        new_term: 8,
    };

    let outcome = RollbackCoordinator::run(&req, &mut node).expect("clean rejoin");
    assert_eq!(outcome.tail_lsns, 0);
    assert!(!outcome.event_fired);
    assert!(outcome.rollback_file.is_none());
    assert_eq!(node.timeline, vec![180, 200], "timeline untouched");
    assert!(matches!(node.role, Some(NodeRole::Replica { .. })));
}
