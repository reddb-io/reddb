//! Browser storage and language-front-end bindings. Query execution and
//! server synchronization are deliberately not advertised by these bindings.

#[cfg(target_arch = "wasm32")]
mod browser;
