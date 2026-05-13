//! Documentation conformance for the table-row MVCC contract.
//!
//! This guards against public docs accidentally broadening the ADR 0014
//! table-row guarantee to every RedDB model.

const TRANSACTIONS_DOC: &str = include_str!("../docs/query/transactions.md");
const LIMITATIONS_DOC: &str = include_str!("../docs/reference/limitations.md");

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected documentation to contain: {needle}"
    );
}

fn assert_not_contains(haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "documentation must not overclaim with: {needle}"
    );
}

#[test]
fn transaction_docs_name_supported_table_row_guarantees() {
    for required in [
        "snapshot isolation",
        "table rows use stable logical identity",
        "versioned `UPDATE`",
        "tombstone\n`DELETE`",
        "first-committer-wins conflict checks",
        "atomic `TxCommitBatch`\nrecovery",
        "Commit recovery boundary",
        "Recovery applies only complete, valid commit batches",
        "manual `VACUUM`",
        "serialization conflict",
        "history store",
    ] {
        assert_contains(TRANSACTIONS_DOC, required);
    }
}

#[test]
fn transaction_docs_name_explicit_deferrals() {
    for required in [
        "Full multi-model rollout is out of scope",
        "`SERIALIZABLE` isolation and SSI are out of scope",
        "An autovacuum daemon is out of scope",
        "Historical secondary indexes are out\n  of scope",
        "cross-node transaction atomicity are out of scope",
    ] {
        assert_contains(TRANSACTIONS_DOC, required);
    }
}

#[test]
fn transaction_docs_link_prd_and_adr() {
    assert_contains(
        TRANSACTIONS_DOC,
        "../adr/0014-mvcc-history-store-and-transaction-recovery.md",
    );
    assert_contains(
        TRANSACTIONS_DOC,
        "https://github.com/reddb-io/reddb/issues/432",
    );
}

#[test]
fn docs_do_not_claim_history_store_mvcc_for_every_model() {
    for forbidden in [
        "full MVCC visibility",
        "A single `BEGIN / COMMIT` is atomic across every data model",
        "all carry `xmin` / `xmax` headers and honour the same\nvisibility rules",
        "RedDB does all of this natively",
    ] {
        assert_not_contains(TRANSACTIONS_DOC, forbidden);
    }

    assert_contains(
        TRANSACTIONS_DOC,
        "Non-table models keep their existing documented transaction behavior",
    );
    assert_contains(
        LIMITATIONS_DOC,
        "the new history-store MVCC guarantee is table-row-first",
    );
    assert_contains(
        LIMITATIONS_DOC,
        "Non-table models retain their documented behavior until each path adopts the history-store resolver",
    );
}
