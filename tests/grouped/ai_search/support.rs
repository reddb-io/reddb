use std::sync::{Mutex, OnceLock};

#[path = "../../support/mod.rs"]
mod root_support;

pub(crate) use root_support::{persistent_test_runtime, PersistentRuntime};

pub(crate) fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn backend_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}
