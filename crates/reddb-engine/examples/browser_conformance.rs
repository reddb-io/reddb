//! Test-only WASM artifact: execute the same assertions as the native tests.
//! This is built separately and never included in the application WASM bundle.

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/portable.rs"]
mod portable;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runConformance)]
pub fn run_conformance() -> String {
    for (_, run) in portable::CASES {
        run();
    }
    portable::CASES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",")
}
