//! Authority fence for the logical type system (ADR 0052, PRD #1060).
//!
//! `reddb-io-types` is the neutral keystone crate that owns the logical type
//! vocabulary — `Value`, `DataType`, `SqlTypeName`, `TypeModifier`,
//! `TypeCategory`, `ValueError`, `Row` — and the coercion entry points
//! (`coerce`, `find_cast`, the spine resolvers). The server tree must import
//! those items directly from `reddb_types`; it must never *declare* them again.
//!
//! This mirrors the layout-authority prior art in `reddb-file`'s test suite
//! (`tests/layout_authority/boundary.rs`): a mechanical fence that fails the
//! instant a forbidden redeclaration reappears in the server source tree.

use std::fs;
use std::path::{Path, PathBuf};

struct SeededConcept {
    name: &'static str,
    authority_path: &'static str,
    grandfathered_server_paths: &'static [&'static str],
    removal_slice: &'static str,
}

// These are the architectural concepts that were declared on both sides of an
// authority boundary when phase 2 began (#2113) and are still duplicated. Generic cross-domain names
// (`Cursor`, `JsonValue`, `ParseError`, and `Value`) are deliberately absent:
// their declarations represent unrelated concepts and cannot seed this fence.
const SEEDED_CONCEPTS: &[SeededConcept] = &[
    concept(
        "ColumnStats",
        "crates/reddb-rql/src/optimizer/stats.rs",
        &["crates/reddb-server/src/storage/query/planner/cost.rs"],
        "#2165",
    ),
    concept(
        "CompareOp",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/query/executors/subquery.rs"],
        "#2165",
    ),
    concept(
        "EdgeDirection",
        "crates/reddb-rql/src/core.rs",
        &[
            "crates/reddb-server/src/runtime.rs",
            "crates/reddb-server/src/storage/query/rag/unified_adapter.rs",
            "crates/reddb-server/src/storage/unified/index.rs",
        ],
        "#2165",
    ),
    concept(
        "EmbeddedRdbArtifact",
        "crates/reddb-file/src/embedded.rs",
        &["crates/reddb-server/src/storage/embedded.rs"],
        "#2113",
    ),
    concept(
        "EntityType",
        "crates/reddb-rql/src/modes/natural.rs",
        &["crates/reddb-server/src/storage/query/rag/mod.rs"],
        "#2165",
    ),
    concept(
        "EscapeError",
        "crates/reddb-wire/src/sanitizer.rs",
        &["crates/reddb-server/src/server/header_escape_guard.rs"],
        "#2113",
    ),
    concept(
        "Filter",
        "crates/reddb-rql/src/core.rs",
        &[
            "crates/reddb-server/src/storage/query/filter.rs",
            "crates/reddb-server/src/storage/unified/dsl/filters.rs",
        ],
        "#2165",
    ),
    concept(
        "FilterExpr",
        "crates/reddb-rql/src/optimizer/filter_rank.rs",
        &["crates/reddb-server/src/storage/query/engine/op.rs"],
        "#2165",
    ),
    concept(
        "FilterValue",
        "crates/reddb-rql/src/optimizer/filter_rank.rs",
        &["crates/reddb-server/src/storage/unified/dsl/filters.rs"],
        "#2165",
    ),
    concept(
        "Frame",
        "crates/reddb-wire/src/redwire/frame.rs",
        &["crates/reddb-server/src/runtime/ai/sse_frame_encoder.rs"],
        "#2113",
    ),
    concept(
        "GraphQueryBuilder",
        "crates/reddb-rql/src/builders.rs",
        &["crates/reddb-server/src/storage/unified/dsl/builders/graph.rs"],
        "#2165",
    ),
    concept(
        "IsolationLevel",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/transaction/mod.rs"],
        "#2165",
    ),
    concept(
        "JoinCondition",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/query/executors/join.rs"],
        "#2165",
    ),
    concept(
        "JoinType",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/query/executors/join.rs"],
        "#2165",
    ),
    concept(
        "MetadataFilter",
        "crates/reddb-types/src/vector_metadata.rs",
        &[
            "crates/reddb-server/src/storage/unified/devx/query.rs",
            "crates/reddb-server/src/storage/unified/metadata.rs",
        ],
        "#2127",
    ),
    concept(
        "MetadataValue",
        "crates/reddb-types/src/vector_metadata.rs",
        &["crates/reddb-server/src/storage/unified/metadata.rs"],
        "#2127",
    ),
    concept(
        "NodePattern",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/query/rag/unified_adapter.rs"],
        "#2165",
    ),
    concept(
        "PageHeader",
        "crates/reddb-file/src/vector_btree_page_format.rs",
        &["crates/reddb-server/src/storage/engine/page.rs"],
        "#2113",
    ),
    concept(
        "PageType",
        "crates/reddb-file/src/vector_btree_page_format.rs",
        &["crates/reddb-server/src/storage/engine/page.rs"],
        "#2113",
    ),
    concept(
        "ParamValue",
        "crates/reddb-wire/src/query_with_params.rs",
        &["crates/reddb-server/src/runtime/query_request.rs"],
        "#2113",
    ),
    concept(
        "Position",
        "crates/reddb-rql/src/lexer.rs",
        &["crates/reddb-server/src/cluster/ownership.rs"],
        "#2165",
    ),
    concept(
        "PropertyFilter",
        "crates/reddb-rql/src/modes/natural.rs",
        &["crates/reddb-server/src/storage/unified/devx/query.rs"],
        "#2165",
    ),
    concept(
        "QueryIntent",
        "crates/reddb-rql/src/modes/natural.rs",
        &["crates/reddb-server/src/storage/query/rag/mod.rs"],
        "#2165",
    ),
    concept(
        "QueueSide",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/queue/deque.rs"],
        "#2165",
    ),
    concept(
        "SnapshotDescriptor",
        "crates/reddb-file/src/physical_metadata/types.rs",
        &["crates/reddb-server/src/storage/wal/recovery.rs"],
        "#2113",
    ),
    concept(
        "SystemClock",
        "crates/reddb-file/src/clock.rs",
        &[
            "crates/reddb-server/src/runtime/queue_lifecycle.rs",
            "crates/reddb-server/src/server/output_stream.rs",
        ],
        "#2113",
    ),
    concept(
        "TableQueryBuilder",
        "crates/reddb-rql/src/builders.rs",
        &["crates/reddb-server/src/storage/unified/dsl/builders/table.rs"],
        "#2165",
    ),
    concept(
        "TableStats",
        "crates/reddb-rql/src/optimizer/stats.rs",
        &["crates/reddb-server/src/storage/query/planner/cost.rs"],
        "#2165",
    ),
    concept(
        "Token",
        "crates/reddb-rql/src/lexer.rs",
        &["crates/reddb-server/src/cli/token.rs"],
        "#2165",
    ),
    concept(
        "ValueFlag",
        "crates/reddb-file/src/vector_value_codec.rs",
        &["crates/reddb-server/src/storage/query/binary.rs"],
        "#2113",
    ),
    concept(
        "VectorSource",
        "crates/reddb-rql/src/core.rs",
        &["crates/reddb-server/src/storage/vector/introspection.rs"],
        "#2165",
    ),
];

const fn concept(
    name: &'static str,
    authority_path: &'static str,
    grandfathered_server_paths: &'static [&'static str],
    removal_slice: &'static str,
) -> SeededConcept {
    SeededConcept {
        name,
        authority_path,
        grandfathered_server_paths,
        removal_slice,
    }
}

#[derive(Debug)]
struct TypeDeclaration {
    name: String,
    shape: Vec<String>,
    has_distinctive_shape: bool,
}

struct ConceptFingerprint<'a> {
    concept: &'a SeededConcept,
    shape: Vec<String>,
    has_distinctive_shape: bool,
}

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

/// Tokenize enough Rust to compare declaration shapes without depending on a
/// parser implementation. Comments and whitespace disappear, quoted literals
/// stay atomic, and identifiers remain available as semantic anchors.
fn rust_tokens(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
        } else if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut depth = 1;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(text[start..index].to_string());
        } else if bytes[index] == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else {
                    let end = bytes[index] == b'"';
                    index += 1;
                    if end {
                        break;
                    }
                }
            }
            tokens.push(text[start..index].to_string());
        } else {
            tokens.push((bytes[index] as char).to_string());
            index += 1;
        }
    }

    tokens
}

fn type_declarations(text: &str) -> Vec<TypeDeclaration> {
    let tokens = rust_tokens(text);
    let mut declarations = Vec::new();
    let mut index = 0;

    while index + 1 < tokens.len() {
        if tokens[index] != "enum" && tokens[index] != "struct" {
            index += 1;
            continue;
        }

        let name = tokens[index + 1].clone();
        if !name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        {
            index += 1;
            continue;
        }

        let start = index;
        let mut end = index + 2;
        let mut brace_depth = 0usize;
        let mut saw_body = false;
        while end < tokens.len() {
            match tokens[end].as_str() {
                "{" => {
                    saw_body = true;
                    brace_depth += 1;
                }
                "}" if saw_body => {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        end += 1;
                        break;
                    }
                }
                ";" if !saw_body => {
                    end += 1;
                    break;
                }
                _ => {}
            }
            end += 1;
        }

        let shape: Vec<String> = tokens[start..end]
            .iter()
            .map(|token| {
                if token == &name {
                    "$TYPE".to_string()
                } else {
                    token.clone()
                }
            })
            .collect();
        let semantic_anchors = shape
            .iter()
            .filter(|token| {
                token
                    .bytes()
                    .next()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                    && !matches!(token.as_str(), "enum" | "struct" | "pub" | "$TYPE")
            })
            .count();
        declarations.push(TypeDeclaration {
            name,
            shape,
            // A markerless/unit declaration is not concept-identifying. Its
            // seeded name remains blocked, while renamed near-duplicates need
            // the review rule documented in monorepo-structure.md.
            has_distinctive_shape: semantic_anchors >= 2,
        });
        index = end;
    }

    declarations
}

fn concept_fingerprints(root: &Path) -> Vec<ConceptFingerprint<'_>> {
    SEEDED_CONCEPTS
        .iter()
        .map(|concept| {
            let authority_source = read(root.join(concept.authority_path));
            let declaration = type_declarations(non_test_source(&authority_source))
                .into_iter()
                .find(|declaration| declaration.name == concept.name)
                .unwrap_or_else(|| {
                    panic!(
                        "{} must declare seeded concept `{}`",
                        concept.authority_path, concept.name
                    )
                });
            ConceptFingerprint {
                concept,
                shape: declaration.shape,
                has_distinctive_shape: declaration.has_distinctive_shape,
            }
        })
        .collect()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn concept_violations_with_fingerprints(
    root: &Path,
    path: &Path,
    text: &str,
    fingerprints: &[ConceptFingerprint<'_>],
) -> Vec<String> {
    let rel = relative_path(root, path);
    let declarations = type_declarations(non_test_source(text));
    let mut violations = Vec::new();

    for declaration in declarations {
        for fingerprint in fingerprints {
            let concept = fingerprint.concept;
            let same_shape = fingerprint.has_distinctive_shape
                && declaration.has_distinctive_shape
                && declaration.shape == fingerprint.shape;
            let grandfathered = concept.grandfathered_server_paths.contains(&rel.as_str())
                && (declaration.name == concept.name || same_shape);
            if grandfathered {
                continue;
            }

            let blocked_name = declaration.name == concept.name;
            let blocked_shape = same_shape;
            if blocked_name || blocked_shape {
                violations.push(format!(
                    "{rel} declares `{}` as the seeded `{}` concept; use the authority declaration at {} (grandfather removal: {})",
                    declaration.name, concept.name, concept.authority_path, concept.removal_slice
                ));
            }
        }
    }

    violations
}

fn concept_violations_in_source(root: &Path, path: &Path, text: &str) -> Vec<String> {
    let fingerprints = concept_fingerprints(root);
    concept_violations_with_fingerprints(root, path, text, &fingerprints)
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

    let table_rs = read(root.join("crates/reddb-types/src/table.rs"));
    for name in [
        "TableDef",
        "ColumnDef",
        "IndexDef",
        "Constraint",
        "IndexType",
        "ConstraintType",
        "TableDefError",
    ] {
        assert!(
            declares_type(&table_rs, name),
            "reddb-types/src/table.rs must declare `{name}`"
        );
    }
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
        for name in [
            "Value",
            "Row",
            "TableDef",
            "ColumnDef",
            "IndexDef",
            "Constraint",
            "IndexType",
            "ConstraintType",
            "TableDefError",
        ] {
            assert!(
                !declares_type(text, name),
                "{} declares `{name}`; the logical type lives in reddb_types — re-export it",
                rel.display()
            );
        }
    }
}

/// The concept fence blocks the known cross-boundary concepts by both name
/// and exact declaration shape. Existing declarations are temporary,
/// path-specific exceptions; moving or renaming one does not preserve the
/// exception.
#[test]
fn server_must_not_redeclare_seeded_authority_concepts() {
    // 32 concepts seeded at phase-2 start, minus `DistanceMetric`, whose
    // duplicate this slice (#2164) removed. Retiring a concept means deleting
    // its entry, never loosening this count.
    assert_eq!(
        SEEDED_CONCEPTS.len(),
        31,
        "the phase-2 concept seed must stay explicit"
    );

    let root = repo_root();
    let fingerprints = concept_fingerprints(&root);

    for concept in SEEDED_CONCEPTS {
        assert!(
            concept.removal_slice.starts_with('#'),
            "seeded concept `{}` needs a removal-slice pointer",
            concept.name
        );
        for rel in concept.grandfathered_server_paths {
            let source = read(root.join(rel));
            let fingerprint = fingerprints
                .iter()
                .find(|fingerprint| fingerprint.concept.name == concept.name)
                .expect("seeded concept fingerprint");
            assert!(
                type_declarations(non_test_source(&source))
                    .iter()
                    .any(|declaration| {
                        declaration.name == concept.name
                            || (fingerprint.has_distinctive_shape
                                && declaration.has_distinctive_shape
                                && declaration.shape == fingerprint.shape)
                    }),
                "stale grandfather for `{}` at {rel}; remove it from SEEDED_CONCEPTS",
                concept.name
            );
        }
    }

    let server_src = root.join("crates/reddb-server/src");
    let violations: Vec<String> = rust_files_under(&server_src)
        .into_iter()
        .flat_map(|path| {
            concept_violations_with_fingerprints(&root, &path, &read(&path), &fingerprints)
        })
        .collect();

    assert!(
        violations.is_empty(),
        "server authority concept redeclarations:\n{}",
        violations.join("\n")
    );
}

/// Server imports must name the types keystone directly, and the retired
/// `storage::schema` re-export shims must not return.
#[test]
fn server_imports_name_types_keystone_directly() {
    let root = repo_root();
    let schema = root.join("crates/reddb-server/src/storage/schema");
    for shim in [
        "types.rs",
        "value_codec.rs",
        "coerce.rs",
        "polymorphic.rs",
        "function_catalog.rs",
        "canonical_key.rs",
        "table.rs",
        "coercion_spine.rs",
        "operator_catalog.rs",
        "parametric.rs",
        "cast_catalog.rs",
    ] {
        assert!(
            !schema.join(shim).exists(),
            "retired storage/schema/{shim} shim must not exist"
        );
    }

    // The gate matches qualified paths anywhere on the line, not just `use`
    // statements: the shim is only retired once no call site can still name a
    // type through it, however the path is spelled.
    let retired_paths = [
        "crate::storage::schema::",
        "reddb_server::storage::schema::",
        "reddb::storage::schema::",
    ];
    let mut violations = Vec::new();
    for dir in [
        root.join("crates/reddb-server/src"),
        root.join("crates/reddb-server/tests"),
        root.join("crates/reddb-client/src"),
        root.join("crates/reddb-client/tests"),
        root.join("tests"),
        root.join("examples"),
    ] {
        if !dir.exists() {
            continue;
        }
        for path in rust_files_under(&dir) {
            for (line_index, line) in read(&path).lines().enumerate() {
                let line = line.trim_start();
                if line.starts_with("//") {
                    continue;
                }
                if retired_paths.iter().any(|p| line.contains(p)) {
                    let rel = path.strip_prefix(&root).expect("source under repo root");
                    violations.push(format!("{}:{}", rel.display(), line_index + 1));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "call sites still name retired storage::schema paths:\n{}",
        violations.join("\n")
    );

    // The mod-level re-export is the trunk of the shim: while it stands, every
    // qualified path above keeps resolving.
    let mod_rs = read(&schema.join("mod.rs"));
    assert!(
        !mod_rs.contains("pub use reddb_types::"),
        "storage/schema/mod.rs must not re-export the keystone vocabulary"
    );
}

#[test]
fn renamed_seeded_concept_fixture_fails_the_fence() {
    let root = repo_root();
    let fixture_path = root.join("crates/reddb-types/tests/fixtures/renamed_queue_side.rs");
    let violations = concept_violations_in_source(&root, &fixture_path, &read(&fixture_path));

    assert!(
        violations.iter().any(|violation| {
            violation.contains("RenamedQueueEnd") && violation.contains("QueueSide")
        }),
        "renaming QueueSide must not evade the concept fence: {violations:?}"
    );
}
