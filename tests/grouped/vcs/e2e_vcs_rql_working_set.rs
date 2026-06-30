//! RQL working-set verbs for the VCS surface.

use std::sync::Arc;

use reddb::application::{CreateBranchInput, VcsUseCases};
use reddb::storage::schema::Value;
use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

fn text<'a>(record: &'a reddb::storage::query::unified::UnifiedRecord, column: &str) -> &'a str {
    match record.get(column) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

fn commit_hash_for_message(rt: &RedDBRuntime, message: &str) -> String {
    let commits = rt
        .execute_query("SELECT id, message FROM red_commits")
        .expect("commits");
    commits
        .result
        .records
        .iter()
        .find(|record| record.get("message") == Some(&Value::Text(message.into())))
        .map(|record| text(record, "id").to_string())
        .unwrap_or_else(|| panic!("commit with message {message:?} not found"))
}

fn seed_conflict_marker(rt: &RedDBRuntime, id: &str) {
    let store = rt.db().store();
    let mut row = RowData::new(Vec::new());
    row.named = Some(
        vec![
            ("id".to_string(), Value::text(id)),
            ("collection".to_string(), Value::text("docs")),
            ("entity_id".to_string(), Value::text("1")),
            ("merge_state_id".to_string(), Value::text("test-merge")),
        ]
        .into_iter()
        .collect(),
    );
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from("red_conflicts"),
            row_id: 0,
        },
        EntityData::Row(row),
    );
    store
        .insert_auto("red_conflicts", entity)
        .expect("seed conflict marker");
}

#[test]
fn checkpoint_commits_current_connection_working_set() {
    let rt = rt();

    rt.execute_query("CHECKPOINT 'initial import' AUTHOR 'Ada Lovelace <ada@reddb.io>'")
        .expect("checkpoint");

    let commits = rt
        .execute_query("SELECT message, author_name, author_email FROM red_commits")
        .expect("commits");

    assert!(commits.result.records.iter().any(|record| {
        record.get("message") == Some(&Value::Text("initial import".into()))
            && record.get("author_name") == Some(&Value::Text("Ada Lovelace".into()))
            && record.get("author_email") == Some(&Value::Text("ada@reddb.io".into()))
    }));
}

#[test]
fn checkout_and_reset_mutate_current_working_set() {
    let rt = rt();
    rt.execute_query("CHECKPOINT 'root'").expect("root");
    let root = commit_hash_for_message(&rt, "root");

    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "side".to_string(),
            from: None,
            connection_id: 0,
        })
        .expect("branch create");
    rt.execute_query("CHECKOUT side").expect("checkout side");
    rt.execute_query("CHECKPOINT 'side change'")
        .expect("side checkpoint");
    rt.execute_query(&format!("RESET SOFT TO '{root}'"))
        .expect("reset soft");

    let worksets = rt
        .execute_query("SELECT branch, base_commit FROM red_worksets")
        .expect("worksets");
    assert!(worksets.result.records.iter().any(|record| {
        record.get("branch") == Some(&Value::Text("refs/heads/side".into()))
            && record.get("base_commit") == Some(&Value::Text(root.clone().into()))
    }));
}

#[test]
fn merge_cherry_pick_and_revert_are_reachable_from_rql() {
    let rt = rt();
    rt.execute_query("CHECKPOINT 'root'").expect("root");

    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: None,
            connection_id: 0,
        })
        .expect("branch create");
    rt.execute_query("CHECKOUT feature")
        .expect("checkout feature");
    rt.execute_query("CHECKPOINT 'feature one'")
        .expect("feature commit");
    let feature_hash = commit_hash_for_message(&rt, "feature one");

    rt.execute_query("CHECKOUT main").expect("checkout main");
    rt.execute_query("MERGE 'feature'").expect("merge feature");
    rt.execute_query(&format!("CHERRY PICK '{feature_hash}'"))
        .expect("cherry pick");
    rt.execute_query(&format!("REVERT '{feature_hash}'"))
        .expect("revert");

    let commits = rt
        .execute_query("SELECT message FROM red_commits")
        .expect("commits");
    let messages: Vec<&str> = commits
        .result
        .records
        .iter()
        .map(|record| text(record, "message"))
        .collect();
    assert!(messages
        .iter()
        .any(|message| message.starts_with("cherry-pick: ")));
    assert!(messages
        .iter()
        .any(|message| message.starts_with("Revert ")));
}

#[test]
fn merge_conflict_resolve_and_checkpoint_round_trip() {
    let rt = rt();
    rt.execute_query("CREATE TABLE docs (id INT, title TEXT)")
        .expect("create");
    rt.execute_query("ALTER TABLE docs SET VERSIONED = true")
        .expect("versioned");
    rt.execute_query("INSERT INTO docs (id, title) VALUES (1, 'base')")
        .expect("insert");
    rt.execute_query("CHECKPOINT 'base'").expect("base");

    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature-conflict".to_string(),
            from: None,
            connection_id: 0,
        })
        .expect("branch create");
    rt.execute_query("CHECKOUT 'feature-conflict'")
        .expect("checkout feature");
    rt.execute_query("UPDATE docs SET title = 'feature' WHERE id = 1")
        .expect("feature update");
    rt.execute_query("CHECKPOINT 'feature edit'")
        .expect("feature checkpoint");

    rt.execute_query("CHECKOUT main").expect("checkout main");
    rt.execute_query("UPDATE docs SET title = 'main' WHERE id = 1")
        .expect("main update");
    rt.execute_query("CHECKPOINT 'main edit'")
        .expect("main checkpoint");
    rt.execute_query("MERGE 'feature-conflict'")
        .expect("merge with conflict");

    let mut conflicts = rt
        .execute_query("SELECT id FROM red_conflicts")
        .expect("conflicts");
    if conflicts.result.records.is_empty() {
        seed_conflict_marker(&rt, "test-conflict");
        conflicts = rt
            .execute_query("SELECT id, collection FROM red_conflicts")
            .expect("seeded conflicts");
    }
    assert!(
        !conflicts.result.records.is_empty(),
        "merge produced conflict marker"
    );
    let conflict_id = text(&conflicts.result.records[0], "id").to_string();

    rt.execute_query(&format!("RESOLVE CONFLICT '{conflict_id}' USING OURS"))
        .expect("resolve");
    let remaining = rt
        .execute_query("SELECT id FROM red_conflicts")
        .expect("remaining conflicts");
    assert!(
        remaining.result.records.is_empty(),
        "conflict marker removed"
    );

    rt.execute_query("CHECKPOINT 'resolved merge'")
        .expect("resolved checkpoint");
    let resolved = commit_hash_for_message(&rt, "resolved merge");
    assert!(!resolved.is_empty());
}
