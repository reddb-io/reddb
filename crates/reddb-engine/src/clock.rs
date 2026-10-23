//! Monotonic query timing on the host that executes the shared operators.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::time::Instant;

#[cfg(target_arch = "wasm32")]
pub(crate) struct Instant(f64);

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
}

#[cfg(target_arch = "wasm32")]
impl Instant {
    pub(crate) fn now() -> Self {
        Self(performance_now())
    }

    pub(crate) fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64((performance_now() - self.0).max(0.0) / 1000.0)
    }
}

/// Wall-clock seconds for entity, embedding and cross-reference timestamps.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Date, js_name = now)]
    fn date_now() -> f64;
}

/// JS uses milliseconds; persisted native entity timestamps use whole seconds.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::cast_possible_truncation)] // Deliberately discard subsecond precision.
pub(crate) fn current_unix_secs() -> u64 {
    (date_now() / 1000.0) as u64
}
