//! Test-only OPFS fixture; never linked into the application WASM artifact.

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../reddb-engine/tests/support/catalog.rs"]
mod catalog;

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../reddb-engine/tests/support/enforcement.rs"]
mod enforcement;

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/enforcement_restart.rs"]
mod enforcement_restart;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runContractWriteFixture)]
pub fn run_contract_write_fixture(
    handle: browser::SyncFile,
    phase: u32,
) -> Result<u32, wasm_bindgen::JsValue> {
    enforcement_restart::run(browser::BrowserFile::new(handle), phase)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/codec_corpus.rs"]
mod codec_corpus;

#[cfg(target_arch = "wasm32")]
#[path = "../src/browser.rs"]
mod browser;

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/pager_orchestration_restart.rs"]
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
        0 => fixture::create(file),
        1 => fixture::update(file),
        2 => {
            assert!(
                reddb_engine::storage::pager_io::read_page(&mut file, fixture::PAGE_ID, true)
                    .is_err(),
                "fixture has an interrupted home-page write"
            );
            fixture::recover(file)
        }
        3 => fixture::verify(file),
        _ => return Err(js_sys::Error::new("invalid fixture phase").into()),
    };
    result.map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/btree_restart.rs"]
mod tree_fixture;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runBtreeFixture)]
pub fn run_btree_fixture(
    handle: browser::SyncFile,
    phase: u32,
) -> Result<(), wasm_bindgen::JsValue> {
    tree_fixture::run(browser::BrowserFile::new(handle), phase)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../reddb-engine/tests/support/record_corpus.rs"]
mod record_corpus;

#[cfg(target_arch = "wasm32")]
#[path = "../../reddb-engine/tests/support/resident_file_restart.rs"]
mod resident_fixture;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runResidentStoreFixture)]
pub fn run_resident_store_fixture(
    handle: browser::SyncFile,
    phase: u32,
) -> Result<u32, wasm_bindgen::JsValue> {
    resident_fixture::run(browser::BrowserFile::new(handle), phase)
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}
