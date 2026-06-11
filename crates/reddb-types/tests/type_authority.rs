//! Authority fence for the logical type system (ADR 0052, PRD #1060).
//!
//! `reddb-io-types` is the neutral keystone crate that owns the logical type
//! vocabulary — `Value`, `DataType`, `SqlTypeName`, `TypeModifier`,
//! `TypeCategory`, `ValueError`, `Row` — and the coercion entry points
//! (`coerce`, `find_cast`, the spine resolvers). The server tree may only
//! *re-export* those items through its `storage::schema` shim; it must never
//! *declare* them again.
//!
//! This mirrors the layout-authority prior art in `reddb-file`'s test suite
//! (`tests/layout_authority/boundary.rs`): a mechanical fence that fails the
//! instant a forbidden redeclaration reappears in the server source tree.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/reddb-types has workspace root two levels up")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("read {}: {err}", path.as_ref().display()))
}

/// Drop the `#[cfg(test)]` tail so a test module's local fixtures never trip
/// the fence. Matches the `reddb-file` prior art's helper of the same name.
fn non_test_source(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries =
            fs::read_dir(&path).unwrap_or_else(|err| panic!("read_dir {}: {err}", path.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

/// True when `text` *declares* a type named `name` (as opposed to re-exporting
/// it via `pub use`). Trailing-delimiter forms give a cheap word boundary so a
/// longer identifier like `DataTypeRegistry` does not match `DataType`.
fn declares_type(text: &str, name: &str) -> bool {
    ["enum", "struct"].iter().any(|kind| {
        [" ", "{", "<", "("]
            .iter()
            .any(|suffix| text.contains(&format!("{kind} {name}{suffix}")))
    })
}

/// True when `text` declares a free function (or method) named `name`.
/// Re-exports use `pub use`, never `fn name(`, so this only fires on a real
/// redeclaration of a coercion entry point.
fn declares_fn(text: &str, name: &str) -> bool {
    text.contains(&format!("fn {name}("))
}

/// The types crate is the sole declaration site for the logical vocabulary.
/// Anchors the positive side of the boundary so the fence below has meaning.
#[test]
fn types_crate_owns_the_logical_type_system() {
    let root = repo_root();
    let types_rs = read(root.join("crates/reddb-types/src/types.rs"));
    for name in [
        "Value",
        "DataType",
        "TypeModifier",
        "TypeCategory",
        "ValueError",
    ] {
        assert!(
            declares_type(&types_rs, name),
            "reddb-types/src/types.rs must declare the `{name}` enum"
        );
    }
    for name in ["SqlTypeName", "Row"] {
        assert!(
            declares_type(&types_rs, name),
            "reddb-types/src/types.rs must declare the `{name}` struct"
        );
    }

    let coerce_rs = read(root.join("crates/reddb-types/src/coerce.rs"));
    assert!(
        declares_fn(&coerce_rs, "coerce"),
        "reddb-types/src/coerce.rs must declare the `coerce` entry point"
    );
    let cast_rs = read(root.join("crates/reddb-types/src/cast_catalog.rs"));
    assert!(
        declares_fn(&cast_rs, "find_cast"),
        "reddb-types/src/cast_catalog.rs must declare the `find_cast` entry point"
    );
    let spine_rs = read(root.join("crates/reddb-types/src/coercion_spine.rs"));
    assert!(
        declares_fn(&spine_rs, "resolve_function"),
        "reddb-types/src/coercion_spine.rs must declare the `resolve_function` entry point"
    );
}

/// The fence: the server source tree must never redeclare a logical
/// type-system item. Reintroduce any declaration below and this test fails.
#[test]
fn server_must_not_redeclare_the_logical_type_system() {
    let root = repo_root();
    let server_src = root.join("crates/reddb-server/src");

    // Distinctive type-system names — they have zero legitimate collisions in
    // the server, so the fence applies tree-wide.
    const TYPE_NAMES: &[&str] = &[
        "DataType",
        "SqlTypeName",
        "TypeModifier",
        "TypeCategory",
        "ValueError",
    ];
    // Coercion entry points re-homed into reddb-types (ADR 0052).
    const COERCION_FNS: &[&str] = &[
        "coerce",
        "coerce_via_catalog",
        "find_cast",
        "can_implicit_cast",
        "can_explicit_cast",
        "can_assignment_cast",
        "resolve_function",
        "resolve_binop",
        "resolve_cast",
    ];

    for path in rust_files_under(&server_src) {
        let raw = read(&path);
        let text = non_test_source(&raw);
        let rel = path.strip_prefix(&root).unwrap_or(path.as_path());

        for name in TYPE_NAMES {
            assert!(
                !declares_type(text, name),
                "{} declares `{name}`; re-export `reddb_types::{name}` instead of redeclaring it",
                rel.display()
            );
        }
        for name in COERCION_FNS {
            assert!(
                !declares_fn(text, name),
                "{} declares coercion entry point `{name}`; call `reddb_types::{name}` instead",
                rel.display()
            );
        }
    }

    // `Value` and `Row` are generic names the server legitimately reuses for
    // unrelated domains — the JSON value (`serde_json.rs`) and the graph
    // binding value (`storage/query/engine/binding.rs`). The *logical* SQL
    // `Value`/`Row` historically lived in `storage::schema`, which is now a
    // pure re-export shim. Fence those two names there: the only way to
    // reintroduce the logical type into the server is to redeclare it in the
    // schema module, and this catches exactly that.
    let schema = server_src.join("storage/schema");
    for path in rust_files_under(&schema) {
        let raw = read(&path);
        let text = non_test_source(&raw);
        let rel = path.strip_prefix(&root).unwrap_or(path.as_path());
        for name in ["Value", "Row"] {
            assert!(
                !declares_type(text, name),
                "{} declares `{name}`; the logical type lives in reddb_types — re-export it",
                rel.display()
            );
        }
    }
}

/// The `storage::schema` shims must stay pure re-exports of the keystone crate.
/// Guards the boundary from the positive side: if a shim is ever replaced by a
/// real declaration, its `pub use reddb_types::` line disappears and this fails.
#[test]
fn schema_shims_reexport_from_types_crate() {
    let root = repo_root();
    let schema = root.join("crates/reddb-server/src/storage/schema");
    for shim in [
        "types.rs",
        "coerce.rs",
        "cast_catalog.rs",
        "coercion_spine.rs",
        "function_catalog.rs",
        "operator_catalog.rs",
        "value_codec.rs",
    ] {
        let text = read(schema.join(shim));
        assert!(
            text.contains("pub use reddb_types::"),
            "storage/schema/{shim} must re-export from reddb_types, not declare types locally"
        );
    }
}
