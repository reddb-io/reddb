//! Browser storage and shared scalar-execution bindings. Storage-backed
//! query execution and server synchronization remain pending.

#[cfg(target_arch = "wasm32")]
mod browser;
