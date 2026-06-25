//! Agent-facing type & multi-model knowledge reference — generated from the
//! engine's own type-system authorities (ADR 0061, "Agent-Facing Knowledge &
//! MCP Surface").
//!
//! Where [`crate::knowledge`]'s sibling in `reddb-io-rql` emits the RQL grammar
//! surface, this module emits the **value-type catalog** and the **multi-model
//! map**. The volatile facts come straight from the source of truth so the
//! reference cannot drift from the engine:
//!
//! - the value types come from the three type-system authorities in this crate
//!   — [`function_catalog::FUNCTION_CATALOG`], [`operator_catalog::OPERATOR_CATALOG`],
//!   and [`cast_catalog::CAST_CATALOG`] (every concrete type they reference), and
//! - the operators and casts are likewise read live from those tables.
//!
//! Layered over that per-model type catalog is the **multi-model map**: the
//! paradigms RedDB stores (documents, key-value, queues, graph nodes/edges,
//! vault secrets, config, and RQL-tabular collections). The map is the
//! conceptual narrative — hand-authored because it is judgment, not extractable
//! — but it points into the generated type catalog by [`TypeCategory`], so the
//! two layers stay coupled.
//!
//! Nothing here hand-maintains *which* value types exist — that is read from
//! the catalogs above. The same generated content is served two ways from this
//! one source: as the `reddb://knowledge/types` MCP resource and as the type
//! section of the generated `docs/llms.txt`. The anti-drift tests at the bottom
//! pin the "generated, exhaustive, in-sync" contract.

use crate::cast_catalog::{CastContext, CAST_CATALOG};
use crate::function_catalog::FUNCTION_CATALOG;
use crate::operator_catalog::OPERATOR_CATALOG;
use crate::types::{DataType, TypeCategory};

/// Canonical URI for the type & multi-model knowledge resource served over MCP.
pub const RESOURCE_URI: &str = "reddb://knowledge/types";

/// Short human title for the type knowledge resource.
pub const RESOURCE_TITLE: &str = "RedDB Type & Multi-Model Reference";

/// One-line description of the type knowledge resource.
pub const RESOURCE_DESCRIPTION: &str =
    "Generated value-type catalog (function/operator/cast authorities) plus the multi-model map.";

/// Markers delimiting the generated type block inside `docs/llms.txt`. The
/// `docs/llms.txt` sync test reads the text between these markers and asserts
/// it equals [`type_reference_markdown`], so the file is generated, not
/// hand-maintained.
pub const LLMS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: types -->";
/// Closing marker for the generated type block in `docs/llms.txt`.
pub const LLMS_END_MARKER: &str = "<!-- END GENERATED: types -->";

/// One paradigm in RedDB's multi-model surface. The narrative (`summary`) is
/// hand-authored judgment; `categories` points into the generated value-type
/// catalog so the map is layered over — not duplicated from — the type system.
pub struct ModelParadigm {
    /// Display name of the paradigm (e.g. "Documents", "Graph nodes & edges").
    pub name: &'static str,
    /// One-sentence description of what the paradigm stores and how it is shaped.
    pub summary: &'static str,
    /// The [`TypeCategory`] families this paradigm predominantly holds, linking
    /// the map back into the per-model type catalog above it.
    pub categories: &'static [TypeCategory],
}

/// The multi-model map: the paradigms RedDB stores, layered over the value-type
/// catalog. Hand-authored narrative (ADR 0061 §3), but each entry references the
/// generated [`TypeCategory`] families it predominantly holds.
pub const MULTI_MODEL_MAP: &[ModelParadigm] = &[
    ModelParadigm {
        name: "Documents",
        summary: "Schemaless JSON-shaped entities addressed by collection + entity id; \
nested fields are typed value-by-value from the catalog below.",
        categories: &[
            TypeCategory::Json,
            TypeCategory::String,
            TypeCategory::Numeric,
            TypeCategory::Boolean,
        ],
    },
    ModelParadigm {
        name: "Key-value",
        summary: "Flat collection + key → value pairs for caches and counters; any \
catalogued value type can be the stored payload.",
        categories: &[
            TypeCategory::String,
            TypeCategory::Numeric,
            TypeCategory::Json,
        ],
    },
    ModelParadigm {
        name: "Queues",
        summary: "Ordered FIFO/priority message streams (LPUSH/RPUSH/LPOP/RPOP, ACK/NACK); \
each message body is a catalogued value, usually text or JSON.",
        categories: &[
            TypeCategory::String,
            TypeCategory::Json,
            TypeCategory::TimeSpan,
        ],
    },
    ModelParadigm {
        name: "Graph nodes & edges",
        summary: "Property graph of nodes and edges; references between them are first-class \
value types and properties draw from the full catalog.",
        categories: &[
            TypeCategory::Reference,
            TypeCategory::String,
            TypeCategory::Numeric,
        ],
    },
    ModelParadigm {
        name: "Vault secrets",
        summary: "Encrypted secrets and password hashes that the expression layer treats as \
opaque — coercion must be opted into explicitly.",
        categories: &[TypeCategory::Opaque, TypeCategory::String],
    },
    ModelParadigm {
        name: "Config",
        summary: "Hierarchical, resolvable configuration entries; values are catalogued \
scalars and JSON resolved per environment.",
        categories: &[
            TypeCategory::String,
            TypeCategory::Boolean,
            TypeCategory::Numeric,
            TypeCategory::Json,
        ],
    },
    ModelParadigm {
        name: "RQL-tabular",
        summary: "Relational tables with typed columns queried through RQL; columns bind \
directly to the value types and categories of the catalog below.",
        categories: &[
            TypeCategory::Numeric,
            TypeCategory::String,
            TypeCategory::Boolean,
            TypeCategory::DateTime,
            TypeCategory::Domain,
        ],
    },
];

/// Push a concrete value type into `acc`, skipping the catalog sentinels.
///
/// `DataType::Unknown` (the function-catalog "any" placeholder) and
/// `DataType::Nullable` (the operator-catalog prefix "don't care" marker) are
/// matching markers in the authorities, not real value types, so they are
/// filtered out of the published catalog.
fn push_value_type(acc: &mut Vec<DataType>, candidate: DataType) {
    if matches!(candidate, DataType::Unknown | DataType::Nullable) {
        return;
    }
    if !acc.contains(&candidate) {
        acc.push(candidate);
    }
}

/// Every concrete value type referenced by the type-system authorities
/// (function / operator / cast catalogs), sorted by discriminant and
/// deduplicated.
///
/// This is read live from the three catalogs, so adding a type to any of them
/// flows automatically into the generated reference; the anti-drift test
/// [`tests::catalog_matches_authorities`] independently re-derives the same set
/// and pins the equality.
pub fn catalogued_value_types() -> Vec<DataType> {
    let mut types: Vec<DataType> = Vec::new();
    for entry in FUNCTION_CATALOG {
        for &arg in entry.arg_types {
            push_value_type(&mut types, arg);
        }
        push_value_type(&mut types, entry.return_type);
    }
    for entry in OPERATOR_CATALOG {
        push_value_type(&mut types, entry.lhs_type);
        push_value_type(&mut types, entry.rhs_type);
        push_value_type(&mut types, entry.return_type);
    }
    for entry in CAST_CATALOG {
        push_value_type(&mut types, entry.src);
        push_value_type(&mut types, entry.target);
    }
    types.sort_by_key(|ty| *ty as u8);
    types
}

/// Distinct operator symbols, sorted, taken from the engine's static
/// [`OPERATOR_CATALOG`]. The catalog carries one row per overload, so a symbol
/// (e.g. `-`, which is both infix and prefix) can appear several times — this
/// collapses them.
pub fn operator_symbols() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = OPERATOR_CATALOG.iter().map(|entry| entry.name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The implicit (always-allowed, lossless) casts the engine inserts silently,
/// sorted by `(src, target)` discriminant. These are the widenings an agent can
/// rely on without writing `CAST(...)`.
pub fn implicit_casts() -> Vec<(DataType, DataType)> {
    let mut pairs: Vec<(DataType, DataType)> = CAST_CATALOG
        .iter()
        .filter(|cast| cast.context == CastContext::Implicit)
        .map(|cast| (cast.src, cast.target))
        .collect();
    pairs.sort_by_key(|(src, target)| (*src as u8, *target as u8));
    pairs
}

/// Human label for a [`TypeCategory`], used by the generated map.
fn category_label(category: TypeCategory) -> &'static str {
    match category {
        TypeCategory::Numeric => "Numeric",
        TypeCategory::String => "String",
        TypeCategory::Boolean => "Boolean",
        TypeCategory::DateTime => "DateTime",
        TypeCategory::TimeSpan => "TimeSpan",
        TypeCategory::Array => "Array",
        TypeCategory::Network => "Network",
        TypeCategory::Geo => "Geo",
        TypeCategory::Domain => "Domain",
        TypeCategory::Uuid => "Uuid",
        TypeCategory::Opaque => "Opaque",
        TypeCategory::Reference => "Reference",
        TypeCategory::Vector => "Vector",
        TypeCategory::Json => "Json",
        TypeCategory::Unknown => "Unknown",
    }
}

/// The order in which value-type categories are presented in the reference.
const CATEGORY_ORDER: &[TypeCategory] = &[
    TypeCategory::Numeric,
    TypeCategory::String,
    TypeCategory::Boolean,
    TypeCategory::DateTime,
    TypeCategory::TimeSpan,
    TypeCategory::Domain,
    TypeCategory::Network,
    TypeCategory::Geo,
    TypeCategory::Uuid,
    TypeCategory::Json,
    TypeCategory::Vector,
    TypeCategory::Array,
    TypeCategory::Reference,
    TypeCategory::Opaque,
];

fn render_code_list<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    items
        .into_iter()
        .map(|item| format!("`{}`", item.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate the canonical type & multi-model reference as Markdown, sourced
/// entirely from the engine's type-system authorities (for the type catalog)
/// plus the hand-authored multi-model narrative. This single string is what the
/// MCP `reddb://knowledge/types` resource serves and what `docs/llms.txt` embeds.
pub fn type_reference_markdown() -> String {
    let types = catalogued_value_types();
    let operators = operator_symbols();
    let casts = implicit_casts();

    let mut out = String::new();
    out.push_str("# RedDB Type & Multi-Model Reference\n\n");
    out.push_str(
        "RedDB is a multi-model store (documents, key-value, queues, graph, vault, \
config, and RQL-tabular collections) layered over one logical type system.\n\n",
    );
    out.push_str(
        "This reference is generated from the `reddb-io-types` function, operator, and \
cast catalogs. Do not edit by hand — regenerate from the engine.\n\n",
    );

    // ── Value types, grouped by category ──
    out.push_str(&format!("## Value types ({})\n\n", types.len()));
    out.push_str(
        "Every concrete value type the engine's type-system authorities reference, \
grouped by coercion category:\n\n",
    );
    for &category in CATEGORY_ORDER {
        let mut names: Vec<String> = types
            .iter()
            .filter(|ty| ty.category() == category)
            .map(|ty| ty.to_string())
            .collect();
        names.dedup();
        if names.is_empty() {
            continue;
        }
        out.push_str(&format!("### {} types\n\n", category_label(category)));
        out.push_str(&render_code_list(&names));
        out.push_str("\n\n");
    }

    // ── Operators ──
    out.push_str(&format!("## Operators ({})\n\n", operators.len()));
    out.push_str("The type system resolves these built-in operators:\n\n");
    out.push_str(&render_code_list(&operators));
    out.push_str("\n\n");

    // ── Implicit casts ──
    out.push_str(&format!("## Implicit casts ({})\n\n", casts.len()));
    out.push_str(
        "Lossless widenings the engine inserts silently — usable anywhere without an \
explicit `CAST`:\n\n",
    );
    for (src, target) in &casts {
        out.push_str(&format!("- `{src}` → `{target}`\n"));
    }
    out.push('\n');

    // ── Multi-model map, layered over the type catalog ──
    out.push_str("## Multi-model map\n\n");
    out.push_str(
        "RedDB stores several paradigms over the value-type catalog above. Each \
paradigm's `Type families` point back into that catalog by category:\n\n",
    );
    for paradigm in MULTI_MODEL_MAP {
        out.push_str(&format!("### {}\n\n", paradigm.name));
        out.push_str(paradigm.summary);
        out.push_str("\n\n");
        let families: Vec<&str> = paradigm
            .categories
            .iter()
            .map(|&category| category_label(category))
            .collect();
        out.push_str(&format!(
            "Type families: {}\n\n",
            render_code_list(&families)
        ));
    }

    // Trim the trailing blank line so the body ends with exactly one newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The type block as embedded in `docs/llms.txt`: the generated reference fenced
/// by the begin/end markers. Emitting the markers here keeps `docs/llms.txt` and
/// the MCP resource fed by one source.
pub fn type_llms_section() -> String {
    format!(
        "{begin}\n{body}\n{end}",
        begin = LLMS_BEGIN_MARKER,
        body = type_reference_markdown(),
        end = LLMS_END_MARKER,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independently re-derive the concrete value types from the three
    /// authorities and assert the published catalog equals that set — the
    /// anti-drift guarantee: the reference stays in sync with the
    /// function/operator/cast catalogs.
    #[test]
    fn catalog_matches_authorities() {
        let mut expected: Vec<DataType> = Vec::new();
        let mut record = |ty: DataType| {
            if matches!(ty, DataType::Unknown | DataType::Nullable) {
                return;
            }
            if !expected.contains(&ty) {
                expected.push(ty);
            }
        };
        for entry in FUNCTION_CATALOG {
            for &arg in entry.arg_types {
                record(arg);
            }
            record(entry.return_type);
        }
        for entry in OPERATOR_CATALOG {
            record(entry.lhs_type);
            record(entry.rhs_type);
            record(entry.return_type);
        }
        for entry in CAST_CATALOG {
            record(entry.src);
            record(entry.target);
        }
        expected.sort_by_key(|ty| *ty as u8);

        assert_eq!(
            catalogued_value_types(),
            expected,
            "the published value-type catalog drifted from the function/operator/cast \
authorities in reddb-io-types"
        );
    }

    /// The published catalog excludes the catalog sentinels and is non-empty.
    #[test]
    fn catalog_excludes_sentinels() {
        let types = catalogued_value_types();
        assert!(!types.is_empty(), "value-type catalog must not be empty");
        assert!(
            !types.contains(&DataType::Unknown),
            "Unknown is a catalog placeholder, not a value type"
        );
        assert!(
            !types.contains(&DataType::Nullable),
            "Nullable is a prefix-operator marker, not a value type"
        );
    }

    /// The catalog is sorted by discriminant, so the generated reference is
    /// stable and reviewable.
    #[test]
    fn catalog_is_sorted_and_unique() {
        let types = catalogued_value_types();
        let mut sorted = types.clone();
        sorted.sort_by_key(|ty| *ty as u8);
        assert_eq!(types, sorted, "catalogued_value_types must be sorted");
        let mut deduped = types.clone();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            types.len(),
            "catalog must not contain duplicates"
        );
    }

    /// authorities ⊆ reference: every catalogued value type appears, by its
    /// display name, in the generated reference.
    #[test]
    fn reference_lists_every_value_type() {
        let reference = type_reference_markdown();
        for ty in catalogued_value_types() {
            assert!(
                reference.contains(&format!("`{ty}`")),
                "value type {ty} from the catalogs is missing from the generated type \
reference"
            );
        }
    }

    /// Every operator symbol from the catalog appears in the reference.
    #[test]
    fn reference_lists_every_operator() {
        let reference = type_reference_markdown();
        for symbol in operator_symbols() {
            assert!(
                reference.contains(&format!("`{symbol}`")),
                "operator {symbol:?} is missing from the generated type reference"
            );
        }
    }

    /// The multi-model map enumerates all seven paradigms RedDB stores, and each
    /// is layered over the type catalog by at least one category family.
    #[test]
    fn multi_model_map_covers_every_paradigm() {
        let names: Vec<&str> = MULTI_MODEL_MAP.iter().map(|m| m.name).collect();
        for expected in [
            "Documents",
            "Key-value",
            "Queues",
            "Graph nodes & edges",
            "Vault secrets",
            "Config",
            "RQL-tabular",
        ] {
            assert!(
                names.contains(&expected),
                "multi-model map is missing the {expected:?} paradigm"
            );
        }
        for paradigm in MULTI_MODEL_MAP {
            assert!(
                !paradigm.categories.is_empty(),
                "paradigm {:?} must link to at least one type category",
                paradigm.name
            );
        }
    }

    /// Every paradigm and its category families render into the reference.
    #[test]
    fn reference_includes_multi_model_map() {
        let reference = type_reference_markdown();
        for paradigm in MULTI_MODEL_MAP {
            assert!(
                reference.contains(paradigm.name),
                "paradigm {:?} is missing from the generated reference",
                paradigm.name
            );
            for &category in paradigm.categories {
                assert!(
                    reference.contains(category_label(category)),
                    "category {:?} for paradigm {:?} is missing from the reference",
                    category_label(category),
                    paradigm.name
                );
            }
        }
    }

    /// The reference is deterministic (pure function of the catalogs + map).
    #[test]
    fn reference_is_deterministic() {
        assert_eq!(type_reference_markdown(), type_reference_markdown());
    }

    /// The `docs/llms.txt` block wraps exactly the reference between markers.
    #[test]
    fn llms_section_wraps_reference() {
        let section = type_llms_section();
        assert!(section.starts_with(LLMS_BEGIN_MARKER));
        assert!(section.ends_with(LLMS_END_MARKER));
        assert!(section.contains(&type_reference_markdown()));
    }
}
