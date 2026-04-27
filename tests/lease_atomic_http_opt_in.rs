//! Runtime contract for `AtomicHttpBackend::try_new`.
//!
//! Cluster 3 of the trait split: the HTTP backend can carry CAS
//! semantics only when the operator explicitly opts in via
//! `HttpBackendConfig::conditional_writes = true`. The constructor
//! must refuse to build a CAS-capable backend over a config that
//! hasn't been confirmed compatible with RFC 7232 preconditions.
//!
//! No network is touched — `try_new` validates the config struct.

use reddb::storage::backend::{AtomicHttpBackend, BackendError, HttpBackendConfig};

fn base_config() -> HttpBackendConfig {
    HttpBackendConfig::new("https://storage.example.com").with_prefix("databases/")
}

#[test]
fn try_new_rejects_config_without_conditional_writes() {
    let cfg = base_config();
    assert!(
        !cfg.conditional_writes,
        "default config must have conditional_writes=false"
    );

    let err = AtomicHttpBackend::try_new(cfg)
        .err()
        .expect("try_new must fail when conditional_writes is false");

    match err {
        BackendError::Config(msg) => assert!(
            msg.contains("conditional_writes"),
            "error must mention the missing opt-in: got {msg:?}"
        ),
        other => panic!("expected BackendError::Config, got {other:?}"),
    }
}

#[test]
fn try_new_rejects_explicit_false_opt_out() {
    // Re-enable then disable: the constructor must trust the final
    // value, not the chain of builder calls.
    let cfg = base_config()
        .with_conditional_writes(true)
        .with_conditional_writes(false);
    assert!(AtomicHttpBackend::try_new(cfg).is_err());
}

#[test]
fn try_new_succeeds_when_conditional_writes_enabled() {
    let cfg = base_config().with_conditional_writes(true);
    let backend =
        AtomicHttpBackend::try_new(cfg).expect("try_new must succeed once the operator opts in");
    // Sanity: the wrapper exposes the inner HttpBackend so the
    // snapshot-transport surface remains reachable.
    let _ = backend.inner();
}
