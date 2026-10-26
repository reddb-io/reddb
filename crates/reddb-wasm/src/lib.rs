//! Browser artifact storage, scalar execution and read-only local table SELECT.
//! SQL writes, application transactions and server synchronization remain pending.

#[cfg(target_arch = "wasm32")]
mod browser;
