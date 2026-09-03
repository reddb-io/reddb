//! Connection admission control for the binary-protocol listeners.
//!
//! The HTTP edge bounds in-flight work with a global permit plus a
//! per-principal cap (issues #931 / #934). RedWire and PG-wire had neither:
//! their accept loops spawned a task per connection with no ceiling and no
//! deadline, so a peer could open sockets until the process ran out of file
//! descriptors or memory, or park mid-handshake and hold a task forever.
//! Neither costs the attacker a credential — the caps and the per-request
//! handler deadline only start applying *after* the handshake completes.
//!
//! This module supplies the two missing bounds:
//!
//!   * [`ConnectionAdmission`] — a semaphore sized per listener. A connection
//!     holds its permit for its whole lifetime, so the count bounds
//!     concurrent sessions rather than in-flight requests (a RedWire session
//!     is long-lived, unlike an HTTP request).
//!   * [`handshake_deadline`] — the window in which a freshly accepted
//!     connection must finish TLS negotiation and the protocol handshake.
//!
//! Both are env-tunable so an operator fronting RedDB with their own
//! connection pool can raise them, and both fail *open* on a malformed value
//! rather than refusing to boot.

use std::sync::Arc;
use std::time::Duration;

/// Default ceiling on concurrent sessions per binary-protocol listener.
/// Generous enough that ordinary pooled workloads never see it, small enough
/// that an unauthenticated peer cannot exhaust the process.
const DEFAULT_MAX_CONNECTIONS: usize = 1024;

/// Default window for TLS negotiation plus the protocol handshake.
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 15;

/// Env var overriding the per-listener connection ceiling.
const MAX_CONNECTIONS_ENV: &str = "REDDB_WIRE_MAX_CONNECTIONS";

/// Env var overriding the handshake deadline, in seconds. `0` disables it.
const HANDSHAKE_TIMEOUT_ENV: &str = "REDDB_WIRE_HANDSHAKE_TIMEOUT_SECS";

/// A permit held for the lifetime of one accepted connection.
pub(crate) type ConnectionPermit = tokio::sync::OwnedSemaphorePermit;

/// Bounds how many connections a listener serves concurrently.
#[derive(Clone)]
pub(crate) struct ConnectionAdmission {
    semaphore: Arc<tokio::sync::Semaphore>,
    transport: &'static str,
}

impl ConnectionAdmission {
    /// Build the limiter for `transport` ("redwire", "pg-wire", ...), reading
    /// the ceiling from the environment once at listener start.
    pub(crate) fn new(transport: &'static str) -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_connections())),
            transport,
        }
    }

    /// Take a permit for a newly accepted connection, or `None` when the
    /// listener is at capacity. Refusing here is deliberate: queueing would
    /// let a flood build an unbounded backlog of parked tasks, which is the
    /// resource we are protecting.
    pub(crate) fn try_admit(&self) -> Option<ConnectionPermit> {
        match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                tracing::warn!(
                    target: "reddb::security",
                    transport = self.transport,
                    limit = max_connections(),
                    "refusing connection: listener at capacity"
                );
                None
            }
        }
    }
}

/// Read the per-listener connection ceiling.
fn max_connections() -> usize {
    // `RED_MAX_CONNECTIONS` is the documented operator cap; the
    // wire-specific name stays as the override for a single listener.
    [
        MAX_CONNECTIONS_ENV,
        "RED_MAX_CONNECTIONS",
        "REDDB_MAX_CONNECTIONS",
    ]
    .iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
    })
    .unwrap_or(DEFAULT_MAX_CONNECTIONS)
}

/// The window a connection has to finish TLS and the protocol handshake.
/// `None` when explicitly disabled with `0`.
pub(crate) fn handshake_deadline() -> Option<Duration> {
    let secs = std::env::var(HANDSHAKE_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Run `future` under the handshake deadline.
///
/// The outer `Result` reports only the deadline: `Err` means the peer ran out
/// of time, `Ok` carries the future's own output untouched (which for these
/// callers is itself a `Result`, with its own protocol error type). Keeping
/// the two apart lets a caller log "timed out" distinctly from "handshake
/// failed", which is the difference between a stalled peer and a bad one.
pub(crate) async fn with_handshake_deadline<F>(future: F) -> std::io::Result<F::Output>
where
    F: std::future::Future,
{
    match handshake_deadline() {
        None => Ok(future.await),
        Some(deadline) => tokio::time::timeout(deadline, future).await.map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("handshake did not complete within {}s", deadline.as_secs()),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_refuses_once_every_permit_is_held() {
        // Hold the whole listener's budget, then confirm the next connection
        // is refused rather than queued — an unbounded queue of parked tasks
        // is the resource this guards.
        let admission = ConnectionAdmission {
            semaphore: Arc::new(tokio::sync::Semaphore::new(2)),
            transport: "test",
        };
        let first = admission.try_admit().expect("first connection admitted");
        let second = admission.try_admit().expect("second connection admitted");
        assert!(admission.try_admit().is_none(), "listener is at capacity");

        // A permit released by a closing connection frees a slot.
        drop(first);
        assert!(admission.try_admit().is_some(), "slot reused after close");
        drop(second);
    }

    #[test]
    fn handshake_deadline_is_overridable_and_disablable() {
        let previous = std::env::var(HANDSHAKE_TIMEOUT_ENV).ok();
        std::env::set_var(HANDSHAKE_TIMEOUT_ENV, "0");
        assert_eq!(handshake_deadline(), None, "0 disables the deadline");
        std::env::set_var(HANDSHAKE_TIMEOUT_ENV, "42");
        assert_eq!(handshake_deadline(), Some(Duration::from_secs(42)));
        std::env::set_var(HANDSHAKE_TIMEOUT_ENV, "not-a-number");
        assert_eq!(
            handshake_deadline(),
            Some(Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS)),
            "a malformed value falls back to the default rather than failing to boot"
        );
        match previous {
            Some(value) => std::env::set_var(HANDSHAKE_TIMEOUT_ENV, value),
            None => std::env::remove_var(HANDSHAKE_TIMEOUT_ENV),
        }
    }
}
