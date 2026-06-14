use std::sync::{Mutex, MutexGuard};

use reddb::{RedDBOptions, RedDBRuntime};

static TIER_STATE_LOCK: Mutex<()> = Mutex::new(());

pub fn tier_state_lock() -> MutexGuard<'static, ()> {
    TIER_STATE_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

pub fn open_in_memory(context: &str) -> RedDBRuntime {
    let _guard = tier_state_lock();
    RedDBRuntime::in_memory().unwrap_or_else(|err| panic!("{context}: {err:?}"))
}

pub fn open_runtime_with_options(options: RedDBOptions, context: &str) -> RedDBRuntime {
    let _guard = tier_state_lock();
    RedDBRuntime::with_options(options).unwrap_or_else(|err| panic!("{context}: {err:?}"))
}
