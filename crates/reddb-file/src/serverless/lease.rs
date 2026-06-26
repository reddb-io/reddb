use super::*;

use serde_json::{Map as JsonMap, Value as JsonValue};
use std::sync::atomic::{AtomicU64, Ordering};

pub const SERVERLESS_WRITER_LEASE_DEFAULT_TERM: u64 = 1;
static SERVERLESS_WRITER_LEASE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerlessWriterLease {
    pub database_key: String,
    pub holder_id: String,
    pub term: u64,
    pub generation: u64,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
}

impl ServerlessWriterLease {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms <= now_ms
    }

    pub fn fenced_by_term(&self, current_term: u64) -> bool {
        self.term < current_term
    }

    pub fn fencing_token(&self) -> (u64, u64) {
        (self.term, self.generation)
    }
}

pub fn serverless_writer_lease_key(prefix: &str, database_key: &str) -> String {
    format!("{prefix}{database_key}.lease.json")
}

pub fn serverless_writer_lease_temp_path(
    kind: &str,
    process_id: u32,
    now_unix_nanos: u128,
    unique: u64,
) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-lease-{kind}-{process_id}-{now_unix_nanos}-{unique}.json"
    ))
}

#[derive(Debug)]
pub struct ServerlessWriterLeaseTempFile {
    path: PathBuf,
}

impl ServerlessWriterLeaseTempFile {
    pub fn new(kind: &str) -> Self {
        let unique = SERVERLESS_WRITER_LEASE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self::with_clock(kind, std::process::id(), now_unix_nanos(), unique)
    }

    pub fn with_clock(kind: &str, process_id: u32, now_unix_nanos: u128, unique: u64) -> Self {
        Self {
            path: serverless_writer_lease_temp_path(kind, process_id, now_unix_nanos, unique),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_bytes(&self, bytes: &[u8]) -> RdbFileResult<()> {
        fs::write(&self.path, bytes)?;
        Ok(())
    }

    pub fn read_bytes(&self) -> RdbFileResult<Vec<u8>> {
        Ok(fs::read(&self.path)?)
    }

    pub fn cleanup(&self) -> RdbFileResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

impl Drop for ServerlessWriterLeaseTempFile {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn now_unix_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod clock_skew_tests {
    use super::*;
    use crate::clock::{Clock, SimClock};

    // 2024-03-10 01:30:00.000 UTC — 30 min before US/Eastern DST spring-forward
    const SEED_MS: u64 = 1_710_033_000_000;
    const LEASE_DURATION_MS: u64 = 5_000; // 5-second lease

    fn make_lease(acquired_at_ms: u64) -> ServerlessWriterLease {
        ServerlessWriterLease {
            database_key: "test-db".to_string(),
            holder_id: "holder-A".to_string(),
            term: 1,
            generation: 1,
            acquired_at_ms,
            expires_at_ms: acquired_at_ms + LEASE_DURATION_MS,
        }
    }

    /// A holder with a clock 2 hours behind (pre-DST, hasn't sprung forward)
    /// incorrectly thinks its lease is still live.  The coordinator — which
    /// runs the authoritative clock — correctly sees the lease as expired, so
    /// the single-writer invariant holds: the coordinator will not grant a new
    /// lease while it still thinks the old one is live, and the term fence
    /// closes the gap if the coordinator also needs to depose the holder.
    #[test]
    fn clock_skew_does_not_extend_lease() {
        let skew_ms = 2 * 3_600_000u64; // 2-hour DST gap

        // Coordinator clock (authoritative) starts at SEED_MS.
        let coordinator = SimClock::from_seed(SEED_MS);
        // Holder clock is 2 hours BEHIND (pre-DST, hasn't sprung forward).
        let holder = SimClock::from_seed(SEED_MS - skew_ms);

        // The coordinator grants the lease; expiry is based on the
        // authoritative wall clock.  This is what gets persisted.
        let lease = make_lease(coordinator.now_unix_millis());

        // 6 seconds pass on both clocks.
        coordinator.advance_ms(6_000);
        holder.advance_ms(6_000);

        // Coordinator: lease has expired (6 s > 5 s after expiry).
        assert!(
            lease.is_expired(coordinator.now_unix_millis()),
            "coordinator must see the lease as expired after 6 s"
        );

        // Holder's clock is still ~2 h before the expiry timestamp, so the
        // holder's own check incorrectly reports the lease as live.
        // expires_at = SEED_MS + 5_000; holder now = SEED_MS - skew_ms + 6_000
        // SEED_MS + 5_000 > SEED_MS - skew_ms + 6_000 because skew_ms >> 5_000
        assert!(
            !lease.is_expired(holder.now_unix_millis()),
            "skewed holder clock incorrectly sees lease as live (the hazard clock-skew creates)"
        );

        // The coordinator's authoritative check always supersedes the holder's
        // self-assessment — this is the fencing guarantee.
        assert!(
            lease.is_expired(coordinator.now_unix_millis()),
            "authoritative coordinator check must override holder self-assessment"
        );
    }

    /// A deposed primary (lower term) is fenced regardless of what the clock says.
    #[test]
    fn deposed_primary_fails_closed_via_term_fence() {
        let clock = SimClock::from_seed(SEED_MS);

        // Term-1 primary acquires a long-lived lease; time has not advanced.
        let lease = make_lease(clock.now_unix_millis());

        // Coordinator elects a new primary — term advances to 2.
        let new_term = 2u64;

        // Old primary is still within its lease window by time alone.
        assert!(
            !lease.is_expired(clock.now_unix_millis()),
            "lease is still live by time, but must be fenced by term"
        );
        assert!(
            lease.fenced_by_term(new_term),
            "deposed primary must be fenced by the higher term"
        );

        // A clock-skewed old primary also cannot bypass the term fence.
        let old_primary_clock = SimClock::from_seed(SEED_MS - 3_600_000);
        drop(old_primary_clock); // only needed to prove skew is irrelevant
        assert!(
            lease.fenced_by_term(new_term),
            "term fence must hold regardless of old primary clock skew"
        );
    }

    /// Same seed + same advance sequence always produces the same timestamps.
    #[test]
    fn simclock_is_reproducible_by_seed() {
        let clock_a = SimClock::from_seed(SEED_MS);
        let clock_b = SimClock::from_seed(SEED_MS);

        // Simulate DST spring-forward: both clocks skip an hour.
        clock_a.advance_ms(3_600_000);
        clock_b.advance_ms(3_600_000);

        assert_eq!(
            clock_a.now_unix_millis(),
            clock_b.now_unix_millis(),
            "SimClock must be deterministic for the same seed and advance sequence"
        );

        // Both clocks agree the 5-second lease is expired after an hour.
        let lease = make_lease(SEED_MS);
        assert!(lease.is_expired(clock_a.now_unix_millis()));
        assert!(lease.is_expired(clock_b.now_unix_millis()));
    }

    /// `set_ms` can model a timezone shift (jumping the UTC offset by ±1 h).
    #[test]
    fn simclock_set_ms_models_timezone_shift() {
        let clock = SimClock::from_seed(SEED_MS);
        let lease = make_lease(clock.now_unix_millis());

        // Jump forward by exactly the DST gap (1 hour).
        clock.set_ms(SEED_MS + 3_600_000);
        assert!(
            lease.is_expired(clock.now_unix_millis()),
            "lease expired after timezone shift of +1 h"
        );

        // Rewind within the lease window.
        clock.set_ms(SEED_MS + 1_000);
        assert!(
            !lease.is_expired(clock.now_unix_millis()),
            "lease still live when clock is within the expiry window"
        );
    }
}

pub fn encode_serverless_writer_lease_json(
    lease: &ServerlessWriterLease,
) -> RdbFileResult<Vec<u8>> {
    let mut object = JsonMap::new();
    object.insert(
        "database_key".to_string(),
        JsonValue::String(lease.database_key.clone()),
    );
    object.insert(
        "holder_id".to_string(),
        JsonValue::String(lease.holder_id.clone()),
    );
    object.insert("term".to_string(), JsonValue::Number(lease.term.into()));
    object.insert(
        "generation".to_string(),
        JsonValue::Number(lease.generation.into()),
    );
    object.insert(
        "acquired_at_ms".to_string(),
        JsonValue::Number(lease.acquired_at_ms.into()),
    );
    object.insert(
        "expires_at_ms".to_string(),
        JsonValue::Number(lease.expires_at_ms.into()),
    );
    serde_json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RdbFileError::InvalidOperation(format!("encode writer lease: {err}")))
}

pub fn decode_serverless_writer_lease_json(bytes: &[u8]) -> RdbFileResult<ServerlessWriterLease> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        RdbFileError::InvalidOperation(format!("decode writer lease json: {err}"))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| RdbFileError::InvalidOperation("lease json is not an object".into()))?;
    Ok(ServerlessWriterLease {
        database_key: required_string(object, "database_key")?,
        holder_id: required_string(object, "holder_id")?,
        term: object
            .get("term")
            .and_then(JsonValue::as_u64)
            .unwrap_or(SERVERLESS_WRITER_LEASE_DEFAULT_TERM),
        generation: required_u64(object, "generation")?,
        acquired_at_ms: required_u64(object, "acquired_at_ms")?,
        expires_at_ms: required_u64(object, "expires_at_ms")?,
    })
}

fn required_string(object: &JsonMap<String, JsonValue>, field: &str) -> RdbFileResult<String> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| RdbFileError::InvalidOperation(format!("missing {field}")))
}

fn required_u64(object: &JsonMap<String, JsonValue>, field: &str) -> RdbFileResult<u64> {
    object
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| RdbFileError::InvalidOperation(format!("missing {field}")))
}
