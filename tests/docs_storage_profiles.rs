const STORAGE_PROFILES_DOC: &str = include_str!("../docs/deployment/storage-profiles.md");
const SIDEBAR: &str = include_str!("../docs/_sidebar.md");
const EMBEDDED_DOC: &str = include_str!("../docs/deployment/embedded.md");
const SERVERLESS_DOC: &str = include_str!("../docs/deployment/serverless.md");
const REPLICATION_DOC: &str = include_str!("../docs/deployment/replication.md");
const MIGRATIONS_OVERVIEW: &str = include_str!("../docs/migrations/overview.md");
const MIGRATIONS_COMMANDS: &str = include_str!("../docs/migrations/commands.md");
const ADR_0003: &str = include_str!("../.red/adr/0003-disk-format-v1.md");
const ADR_0018: &str = include_str!("../.red/adr/0018-tiered-storage-layout.md");
const ADR_0030: &str =
    include_str!("../.red/adr/0030-replication-consistency-and-failover-model.md");
const ADR_0031: &str =
    include_str!("../.red/adr/0031-causal-consistency-bookmarks-and-ttl-replication.md");
const ADR_0032: &str = include_str!("../.red/adr/0032-wal-source-of-truth-and-term-framing.md");

#[test]
fn storage_profile_docs_cover_supported_packaging_and_boundaries() {
    for needle in [
        "embedded",
        "single-file",
        "serverless",
        "segment pack",
        "primary-replica",
        "operational-directory",
        "cluster",
        "range-directory",
        "wal_segments_required = 0",
        "Online conversion while the source database is open",
        "Operational directory back to embedded single-file",
        "backup manifest and WAL archive",
    ] {
        assert!(
            STORAGE_PROFILES_DOC.contains(needle),
            "storage profile docs must mention {needle:?}"
        );
    }
}

#[test]
fn storage_profile_docs_cross_link_relevant_adrs() {
    for adr in [
        "../../.red/adr/0003-disk-format-v1.md",
        "../../.red/adr/0018-tiered-storage-layout.md",
        "../../.red/adr/0030-replication-consistency-and-failover-model.md",
        "../../.red/adr/0031-causal-consistency-bookmarks-and-ttl-replication.md",
        "../../.red/adr/0032-wal-source-of-truth-and-term-framing.md",
    ] {
        assert!(
            STORAGE_PROFILES_DOC.contains(adr),
            "storage profile docs must link {adr}"
        );
    }
}

#[test]
fn storage_profile_adrs_link_back_to_operator_docs() {
    for (name, adr) in [
        ("ADR 0003", ADR_0003),
        ("ADR 0018", ADR_0018),
        ("ADR 0030", ADR_0030),
        ("ADR 0031", ADR_0031),
        ("ADR 0032", ADR_0032),
    ] {
        assert!(
            adr.contains("../../docs/deployment/storage-profiles.md"),
            "{name} must link back to storage profile docs"
        );
    }
}

#[test]
fn storage_profile_entry_points_are_discoverable() {
    assert!(
        SIDEBAR.contains("[Storage Profiles](/deployment/storage-profiles.md)"),
        "sidebar must expose storage profiles"
    );

    for (name, doc) in [
        ("embedded", EMBEDDED_DOC),
        ("serverless", SERVERLESS_DOC),
        ("replication", REPLICATION_DOC),
        ("migrations overview", MIGRATIONS_OVERVIEW),
        ("migrations commands", MIGRATIONS_COMMANDS),
    ] {
        assert!(
            doc.contains("storage-profiles.md"),
            "{name} docs must link to the storage profile entry point"
        );
    }
}
