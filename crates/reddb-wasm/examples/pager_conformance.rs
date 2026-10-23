//! Test-only OPFS fixture; never linked into the application WASM artifact.

#[cfg(target_arch = "wasm32")]
#[path = "../src/browser.rs"]
mod browser;

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/pager_restart.rs"]
mod fixture;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = pagerFixtureOffset)]
pub fn pager_fixture_offset() -> u32 {
    fixture::PAGE_ID
        * u32::try_from(reddb_engine::storage::page::PAGE_SIZE).expect("page size fits u32")
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runPagerFixture)]
pub fn run_pager_fixture(
    handle: browser::SyncFile,
    phase: u32,
) -> Result<(), wasm_bindgen::JsValue> {
    let mut file = browser::BrowserFile::new(handle);
    let result = match phase {
        0 => fixture::create(&mut file),
        1 => fixture::verify(&mut file, b"before").and_then(|()| fixture::update(&mut file)),
        2 => {
            assert!(
                reddb_engine::storage::pager_io::read_page(&mut file, fixture::PAGE_ID, true)
                    .is_err(),
                "fixture has an interrupted home-page write"
            );
            fixture::recover(&mut file)
        }
        _ => return Err(js_sys::Error::new("invalid fixture phase").into()),
    };
    result.map_err(|error| js_sys::Error::new(&error.to_string()).into())
}
