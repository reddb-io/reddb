use crate::common::*;

#[test]
fn context_declares_file_and_protocol_authority_boundary() {
    let root = repo_root();
    let persistence = read(root.join(".red/context/persistence.md"));

    for required in [
        "File/protocol ownership boundary",
        "ADR 0046",
        "`reddb-file` is the authority for file contracts",
        "names, paths, sidecars, manifests, superblocks, WAL artifacts, checkpoint artifacts, and recovery metadata",
        "`reddb-wire` is the authority for communication contracts",
        "frames, codecs, payloads, topology, connection strings, sanitizers, and replication wire messages",
        "`reddb-server` orchestrates runtime, SQL, auth, storage engine policy",
        "must not introduce new persistent file formats or protocol payload formats directly",
    ] {
        assert!(
            persistence.contains(required),
            "persistence context must declare ownership boundary: {required}"
        );
    }
}

#[test]
fn replication_slot_contract_lives_only_in_reddb_file() {
    let root = repo_root();
    let file_slots = read(root.join("crates/reddb-file/src/primary_replica/slots.rs"));
    assert!(
        file_slots.contains("pub struct ReplicationSlot"),
        "reddb-file should own the ReplicationSlot persisted contract"
    );

    let server_root = root.join("crates/reddb-server/src");
    for path in rust_files_under(&server_root) {
        let text = read(&path);
        for forbidden in [
            "pub struct ReplicationSlot",
            "struct ReplicationSlot",
            "pub enum ReplicationSlot",
            "enum ReplicationSlot",
            "type ReplicationSlot",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} must use reddb_file::ReplicationSlot instead of declaring {forbidden:?}",
                path.display()
            );
        }
    }
}
