//! Primary commit policies (PLAN.md Phase 11.4).
//!
//! `Local` — commit returns after the local WAL is durable (default;
//! current behaviour). No replica involvement at commit time.
//!
//! `RemoteWal` — commit returns after the WAL segment containing the
//! transaction has been archived to the remote backend. Bounds
//! durability to "survives a single-node loss as long as the remote
//! is reachable".
//!
//! `AckN(n)` — commit returns after `n` replicas have ack'd the
//! transaction's LSN via `ack_replica_lsn`. `n=0` is equivalent to
//! `Local`. The primary blocks the commit response until the count
//! is met or `RED_REPLICATION_ACK_TIMEOUT_MS` elapses.
//!
//! `Quorum` — future policy backed by `QuorumConfig` once quorum
//! coordination is wired into the commit path. For now this is a
//! marker enum value; the runtime falls back to `Local` semantics
//! and emits a warning at boot when set.
//!
//! In this sprint only the enum + parsing + observability are wired.
//! Actually blocking commits on `RemoteWal` / `AckN` / `Quorum` is
//! out of scope; the write path still returns after local durability
//! regardless of the configured policy. See PLAN.md 11.4 "default v1
//! behavior remains `local`".

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitPolicy {
    Local,
    RemoteWal,
    AckN(u32),
    Quorum,
}

impl Default for CommitPolicy {
    fn default() -> Self {
        Self::Local
    }
}

impl CommitPolicy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::RemoteWal => "remote_wal",
            Self::AckN(_) => "ack_n",
            Self::Quorum => "quorum",
        }
    }

    /// Parse from `RED_PRIMARY_COMMIT_POLICY` env var. Accepts:
    /// `local` (default), `remote_wal`, `ack_n=N` (decimal),
    /// `quorum`. Unknown values fall back to `Local` with a warning.
    pub fn from_env() -> Self {
        match std::env::var("RED_PRIMARY_COMMIT_POLICY").ok() {
            Some(raw) => Self::parse(raw.trim()),
            None => Self::Local,
        }
    }

    pub fn parse(raw: &str) -> Self {
        let lower = raw.to_ascii_lowercase();
        if lower == "local" || lower.is_empty() {
            return Self::Local;
        }
        if lower == "remote_wal" {
            return Self::RemoteWal;
        }
        if lower == "quorum" {
            return Self::Quorum;
        }
        if let Some(n_str) = lower.strip_prefix("ack_n=") {
            if let Ok(n) = n_str.parse::<u32>() {
                return Self::AckN(n);
            }
        }
        tracing::warn!(
            target: "reddb::replication::commit_policy",
            value = %raw,
            "unknown RED_PRIMARY_COMMIT_POLICY; falling back to local"
        );
        Self::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local() {
        assert_eq!(CommitPolicy::default(), CommitPolicy::Local);
    }

    #[test]
    fn parse_known_values() {
        assert_eq!(CommitPolicy::parse("local"), CommitPolicy::Local);
        assert_eq!(CommitPolicy::parse("LOCAL"), CommitPolicy::Local);
        assert_eq!(CommitPolicy::parse("remote_wal"), CommitPolicy::RemoteWal);
        assert_eq!(CommitPolicy::parse("quorum"), CommitPolicy::Quorum);
        assert_eq!(CommitPolicy::parse("ack_n=3"), CommitPolicy::AckN(3));
        assert_eq!(CommitPolicy::parse("ack_n=0"), CommitPolicy::AckN(0));
    }

    #[test]
    fn parse_unknown_falls_back_to_local() {
        assert_eq!(CommitPolicy::parse("nonsense"), CommitPolicy::Local);
        assert_eq!(CommitPolicy::parse("ack_n=abc"), CommitPolicy::Local);
        assert_eq!(CommitPolicy::parse(""), CommitPolicy::Local);
    }

    #[test]
    fn label_round_trips_known_values() {
        assert_eq!(CommitPolicy::Local.label(), "local");
        assert_eq!(CommitPolicy::RemoteWal.label(), "remote_wal");
        assert_eq!(CommitPolicy::AckN(5).label(), "ack_n");
        assert_eq!(CommitPolicy::Quorum.label(), "quorum");
    }
}
