//! Layout-authority assertions for the native-artifact payload codecs relocated
//! from `reddb-server` into `reddb-file` under PRD #1050 / ADR 0046:
//! `HNSW`, `IVF1`, `RDGA`, `RDFT`, `RDDP`, and `RTBL`.
//!
//! `reddb-file` must own each codec's encode/decode, and the server must no
//! longer *define* the on-disk framing (magic emission / byte walking) — only
//! consume the codec.

use crate::common::*;

/// `reddb-file` declares the canonical codec surface for each relocated format.
#[test]
fn reddb_file_owns_native_artifact_codecs() {
    let root = repo_root();

    let hnsw = read(root.join("crates/reddb-file/src/hnsw_index_codec.rs"));
    for required in [
        "pub const HNSW_INDEX_MAGIC",
        "pub const HNSW_INDEX_VERSION",
        "pub struct HnswIndexLayout",
        "pub struct HnswNodeLayout",
        "pub fn encode_hnsw_index",
        "pub fn decode_hnsw_index",
        "b\"HNSW\"",
    ] {
        assert!(
            hnsw.contains(required),
            "reddb-file should own the HNSW index codec: {required}"
        );
    }

    let ivf = read(root.join("crates/reddb-file/src/ivf_index_codec.rs"));
    for required in [
        "pub const IVF_INDEX_MAGIC",
        "pub struct IvfIndexLayout",
        "pub struct IvfListLayout",
        "pub fn encode_ivf_index",
        "pub fn decode_ivf_index",
        "b\"IVF1\"",
    ] {
        assert!(
            ivf.contains(required),
            "reddb-file should own the IVF index codec: {required}"
        );
    }

    let native = read(root.join("crates/reddb-file/src/native_artifact_codec.rs"));
    for required in [
        "pub const GRAPH_ADJACENCY_MAGIC",
        "pub const FULLTEXT_INDEX_MAGIC",
        "pub const DOC_PATHVALUE_MAGIC",
        "pub fn encode_graph_adjacency",
        "pub fn decode_graph_adjacency",
        "pub fn encode_fulltext_index",
        "pub fn decode_fulltext_index",
        "pub fn encode_document_pathvalue",
        "pub fn decode_document_pathvalue",
        "b\"RDGA\"",
        "b\"RDFT\"",
        "b\"RDDP\"",
        // RDDP pins entity_id as a fixed little-endian u64.
        "pub entity_id: u64",
    ] {
        assert!(
            native.contains(required),
            "reddb-file should own the native artifact codecs: {required}"
        );
    }

    let table = read(root.join("crates/reddb-file/src/table_def_codec.rs"));
    for required in [
        "pub const TABLE_DEF_MAGIC",
        "pub struct TableDefLayout",
        "pub struct ColumnLayout",
        "pub struct IndexLayout",
        "pub struct ConstraintLayout",
        "pub fn encode_table_def",
        "pub fn decode_table_def",
        "b\"RTBL\"",
    ] {
        assert!(
            table.contains(required),
            "reddb-file should own the RTBL table-def codec: {required}"
        );
    }
}

/// The server must consume the codecs, never re-define the framing. We assert
/// the magic-emitting / magic-matching definitions no longer live in the
/// server engine sources.
#[test]
fn server_does_not_redeclare_native_artifact_payload_formats() {
    let root = repo_root();

    let hnsw = read(root.join("crates/reddb-server/src/storage/engine/hnsw.rs"));
    let ivf = read(root.join("crates/reddb-server/src/storage/engine/ivf.rs"));
    let impl_access =
        read(root.join("crates/reddb-server/src/storage/unified/devx/reddb/impl_access.rs"));
    let table = read(root.join("crates/reddb-server/src/storage/schema/table.rs"));

    let hnsw_src = non_test_source(&hnsw);
    let ivf_src = non_test_source(&ivf);
    let impl_access_src = non_test_source(&impl_access);
    let table_src = non_test_source(&table);

    for (label, src, forbidden) in [
        ("hnsw.rs", hnsw_src, "extend_from_slice(b\"HNSW\")"),
        ("hnsw.rs", hnsw_src, "&bytes[0..4] != b\"HNSW\""),
        ("ivf.rs", ivf_src, "extend_from_slice(b\"IVF1\")"),
        ("ivf.rs", ivf_src, "&bytes[0..4] != b\"IVF1\""),
        (
            "impl_access.rs",
            impl_access_src,
            "extend_from_slice(b\"RDGA\")",
        ),
        (
            "impl_access.rs",
            impl_access_src,
            "extend_from_slice(b\"RDFT\")",
        ),
        (
            "impl_access.rs",
            impl_access_src,
            "extend_from_slice(b\"RDDP\")",
        ),
        ("impl_access.rs", impl_access_src, "!= b\"RDGA\""),
        ("impl_access.rs", impl_access_src, "!= b\"RDFT\""),
        ("impl_access.rs", impl_access_src, "!= b\"RDDP\""),
        ("table.rs", table_src, "extend_from_slice(b\"RTBL\")"),
        ("table.rs", table_src, "!= b\"RTBL\""),
    ] {
        assert!(
            !src.contains(forbidden),
            "{label} must consume the reddb-file codec, not redeclare {forbidden:?}"
        );
    }

    // Positive: the server now calls into the codec.
    assert!(
        hnsw_src.contains("reddb_file::encode_hnsw_index")
            && hnsw_src.contains("reddb_file::decode_hnsw_index"),
        "hnsw.rs must call the reddb-file HNSW codec"
    );
    assert!(
        ivf_src.contains("reddb_file::encode_ivf_index")
            && ivf_src.contains("reddb_file::decode_ivf_index"),
        "ivf.rs must call the reddb-file IVF codec"
    );
    assert!(
        impl_access_src.contains("reddb_file::encode_graph_adjacency")
            && impl_access_src.contains("reddb_file::decode_graph_adjacency")
            && impl_access_src.contains("reddb_file::encode_fulltext_index")
            && impl_access_src.contains("reddb_file::decode_fulltext_index")
            && impl_access_src.contains("reddb_file::encode_document_pathvalue")
            && impl_access_src.contains("reddb_file::decode_document_pathvalue"),
        "impl_access.rs must call the reddb-file native artifact codecs"
    );
    assert!(
        table_src.contains("reddb_file::encode_table_def")
            && table_src.contains("reddb_file::decode_table_def"),
        "table.rs must call the reddb-file RTBL codec"
    );
}
