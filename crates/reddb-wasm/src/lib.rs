//! Browser artifact storage, scalar execution, local table SQL and transactions.
//! Other collection models and server synchronization remain pending.

#[cfg(target_arch = "wasm32")]
mod browser;
