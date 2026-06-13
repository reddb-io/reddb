//! Compile-fail driver for the `AtomicRemoteBackend` trait split.
//!
//! Cluster 3 of the split moved CAS methods off `RemoteBackend` onto
//! `AtomicRemoteBackend: RemoteBackend`, and tightened
//! `LeaseStore::new` to require `Arc<dyn AtomicRemoteBackend>`. The
//! cases below are negative proofs: feeding a non-CAS backend to
//! `LeaseStore::new` must be rejected by the type system, not at the
//! first contended write.
//!
//! Each `.rs` file under `tests/compile_fail/` is a standalone case
//! that `trybuild` compiles in isolation and asserts fails to build.

#[test]
#[cfg(feature = "backend-turso")]
fn lease_store_rejects_non_cas_backends() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/lease_rejects_turso.rs");
}
