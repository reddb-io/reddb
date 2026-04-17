//! Unit coverage for the `hot_update` decision helper.
//!
//! Lives as an integration test because the lib-test target has
//! pre-existing unrelated compile errors we're not fixing here.

use std::collections::HashSet;

use reddb::storage::engine::hot_update::{decide, HotUpdateInputs};

fn hs(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_indexed_cols_modified_and_fits_page_allows_hot() {
    let indexed = hs(&["email", "org_id"]);
    let modified = hs(&["last_login_at"]);
    let d = decide(&HotUpdateInputs {
        collection: "users",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 100,
        page_free_space: 4096,
    });
    assert!(d.can_hot);
    assert_eq!(d.indexed_blocker, None);
}

#[test]
fn indexed_column_modified_blocks_hot() {
    let indexed = hs(&["email", "org_id"]);
    let modified = hs(&["email"]);
    let d = decide(&HotUpdateInputs {
        collection: "users",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 100,
        page_free_space: 4096,
    });
    assert!(!d.can_hot);
    assert_eq!(d.indexed_blocker.as_deref(), Some("email"));
}

#[test]
fn new_tuple_too_large_blocks_hot() {
    let indexed = hs(&["id"]);
    let modified = hs(&["body"]);
    let d = decide(&HotUpdateInputs {
        collection: "docs",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 5000,
        page_free_space: 4096,
    });
    assert!(!d.can_hot);
    assert_eq!(d.indexed_blocker, None);
}

#[test]
fn unlimited_free_space_bypasses_fit_check() {
    let indexed = hs(&[]);
    let modified = hs(&["v"]);
    let d = decide(&HotUpdateInputs {
        collection: "t",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 999_999_999,
        page_free_space: usize::MAX,
    });
    assert!(d.can_hot);
}

#[test]
fn empty_modified_columns_trivially_passes_the_index_gate() {
    let indexed = hs(&["email"]);
    let modified = hs(&[]);
    let d = decide(&HotUpdateInputs {
        collection: "users",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 50,
        page_free_space: 4096,
    });
    assert!(d.can_hot);
    assert_eq!(d.indexed_blocker, None);
}

#[test]
fn indexed_blocker_picks_some_modified_indexed_column() {
    let indexed = hs(&["a", "b", "c"]);
    let modified = hs(&["a", "b"]);
    let d = decide(&HotUpdateInputs {
        collection: "t",
        indexed_columns: &indexed,
        modified_columns: &modified,
        new_tuple_size: 50,
        page_free_space: 4096,
    });
    assert!(!d.can_hot);
    let blocker = d.indexed_blocker.expect("must have a blocker");
    assert!(blocker == "a" || blocker == "b", "got {blocker}");
}
