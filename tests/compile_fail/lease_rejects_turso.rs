//! Compile-fail: `LeaseStore::new` must refuse a `TursoBackend`.
//!
//! Turso intentionally does **not** implement `AtomicRemoteBackend`
//! (its REST surface cannot enforce CAS atomically), so wiring it
//! into a `LeaseStore` should be a type error — not a runtime
//! `BackendError::Config` at the first contended acquire.

use std::sync::Arc;

use reddb::replication::lease::LeaseStore;
use reddb::storage::backend::{TursoBackend, TursoConfig};

fn main() {
    let backend = TursoBackend::new(TursoConfig::new(
        "https://mydb.turso.io",
        "tok",
    ));
    // Expected error: `TursoBackend` does not implement
    // `AtomicRemoteBackend`, so the `Arc<TursoBackend>` cannot
    // coerce into `Arc<dyn AtomicRemoteBackend>`.
    let _store = LeaseStore::new(Arc::new(backend));
}
