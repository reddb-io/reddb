//! Issue #1569: VCS read surface through RQL virtual tables.

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, CreateTagInput,
    VcsUseCases,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn author() -> Author {
    Author {
        name: "test".to_string(),
        email: "test@reddb.io".to_string(),
    }
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

fn commit(rt: &RedDBRuntime, conn: u64, msg: &str) -> String {
    vcs(rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: msg.to_string(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

fn text<'a>(record: &'a UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text field {field}, got {other:?} in {record:?}"),
    }
}

fn integer(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).expect("fits i64"),
        other => panic!("expected integer field {field}, got {other:?} in {record:?}"),
    }
}

#[test]
fn rql_exposes_vcs_refs_commits_status_and_versioned_collections() {
    let rt = rt();
    let c1 = commit(&rt, 42, "root");
    let c2 = commit(&rt, 42, "second");

    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: Some(c1.clone()),
            connection_id: 42,
        })
        .expect("branch");
    vcs(&rt)
        .tag(CreateTagInput {
            name: "v1".to_string(),
            target: c1.clone(),
            annotation: None,
        })
        .expect("tag");
    rt.execute_query("CREATE TABLE versioned_docs (id INT)")
        .expect("table");
    vcs(&rt)
        .set_versioned("versioned_docs", true)
        .expect("versioned");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 42,
            target: CheckoutTarget::Branch("feature".to_string()),
            force: false,
        })
        .expect("checkout");
    assert!(
        vcs(&rt)
            .branch_list()
            .expect("branch list")
            .iter()
            .any(|reference| reference.name == "refs/heads/feature"),
        "feature branch should exist through the port"
    );

    set_current_connection_id(42);
    let commits = rt
        .execute_query("SELECT hash, message, height FROM red.commits ORDER BY height DESC LIMIT 1")
        .expect("red.commits");
    assert_eq!(commits.result.records.len(), 1);
    assert_eq!(text(&commits.result.records[0], "hash"), c1);
    assert_eq!(text(&commits.result.records[0], "message"), "root");

    let commit_show = rt
        .execute_query(&format!(
            "SELECT hash, message FROM red.commits WHERE hash = '{c2}'"
        ))
        .expect("red.commits hash filter");
    assert_eq!(commit_show.result.records.len(), 1);
    assert_eq!(text(&commit_show.result.records[0], "message"), "second");

    let branches = rt
        .execute_query("SELECT name, target FROM red.branches")
        .expect("red.branches");
    assert!(
        branches
            .result
            .records
            .iter()
            .any(|record| text(record, "name") == "refs/heads/feature"
                && text(record, "target") == c1),
        "expected feature branch in {:?}",
        branches.result.records
    );

    let tags = rt
        .execute_query("SELECT name, target FROM red.tags")
        .expect("red.tags");
    assert!(
        tags.result
            .records
            .iter()
            .any(|record| text(record, "name") == "refs/tags/v1" && text(record, "target") == c1),
        "expected v1 tag in {:?}",
        tags.result.records
    );

    let status = rt
        .execute_query("SELECT connection_id, head_ref, head_commit FROM red.status")
        .expect("red.status");
    assert_eq!(status.result.records.len(), 1);
    assert_eq!(integer(&status.result.records[0], "connection_id"), 42);
    assert_eq!(
        text(&status.result.records[0], "head_ref"),
        "refs/heads/feature"
    );
    assert_eq!(text(&status.result.records[0], "head_commit"), c1);

    let versioned = rt
        .execute_query("SELECT collection FROM red.versioned")
        .expect("red.versioned");
    assert_eq!(versioned.result.records.len(), 1);
    assert_eq!(
        text(&versioned.result.records[0], "collection"),
        "versioned_docs"
    );

    let diff = rt
        .execute_query(&format!("SELECT * FROM red.diff('{c1}', '{c2}')"))
        .expect("red.diff");
    assert_eq!(
        diff.result.columns,
        vec![
            "from".to_string(),
            "to".to_string(),
            "collection".to_string(),
            "entity_id".to_string(),
            "change".to_string(),
            "before".to_string(),
            "after".to_string(),
        ]
    );
    assert!(diff.result.records.is_empty());

    let lca = rt
        .execute_query(&format!(
            "SELECT red.lca('{c1}', '{c2}') AS lca FROM red.status"
        ))
        .expect("red.lca");
    assert_eq!(lca.result.records.len(), 1);
    assert_eq!(text(&lca.result.records[0], "lca"), c1);
    clear_current_connection_id();
}
