//! Phase 7: opt-in per collection. Default is non-versioned; AS OF
//! against a non-opted collection must error; merge / diff skip
//! non-versioned collections entirely.

use std::sync::Arc;

use reddb::application::VcsUseCases;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

#[test]
fn collections_are_non_versioned_by_default() {
    let rt = rt();
    // Creating a user collection implicitly — insert a row via SQL.
    rt.execute_query("CREATE TABLE sessions (id INT, user TEXT)")
        .expect("create table");
    assert!(!vcs(&rt).is_versioned("sessions").unwrap());
    let list = vcs(&rt).list_versioned().unwrap();
    assert!(list.is_empty(), "no user collection should be opted in");
}

#[test]
fn set_versioned_on_and_off() {
    let rt = rt();
    rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
        .unwrap();
    vcs(&rt).set_versioned("users", true).unwrap();
    assert!(vcs(&rt).is_versioned("users").unwrap());
    assert_eq!(
        vcs(&rt).list_versioned().unwrap(),
        vec!["users".to_string()]
    );

    // Toggle off — row deleted, list empty.
    vcs(&rt).set_versioned("users", false).unwrap();
    assert!(!vcs(&rt).is_versioned("users").unwrap());
    assert!(vcs(&rt).list_versioned().unwrap().is_empty());
}

#[test]
fn opt_in_is_idempotent() {
    let rt = rt();
    rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
        .unwrap();
    for _ in 0..3 {
        vcs(&rt).set_versioned("users", true).unwrap();
    }
    let list = vcs(&rt).list_versioned().unwrap();
    assert_eq!(list, vec!["users".to_string()], "no duplicates: {:?}", list);
}

#[test]
fn internal_red_collections_cannot_be_versioned() {
    let rt = rt();
    let err = vcs(&rt)
        .set_versioned("red_commits", true)
        .expect_err("internal collection refused");
    assert!(format!("{err}").contains("internal collection"));
}

#[test]
fn as_of_errors_on_unversioned_table() {
    let rt = rt();
    rt.execute_query("CREATE TABLE sessions (id INT, user TEXT)")
        .unwrap();
    // Make a commit so there's a HEAD to reference.
    vcs(&rt)
        .commit(reddb::application::CreateCommitInput {
            connection_id: 1,
            message: "c1".to_string(),
            author: reddb::application::Author {
                name: "t".to_string(),
                email: "t@x".to_string(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap();

    let err = rt
        .execute_query("SELECT * FROM sessions AS OF BRANCH 'main'")
        .expect_err("unversioned table refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("versioned collection") || msg.contains("opted in"),
        "got: {msg}"
    );
}

#[test]
fn alter_table_set_versioned_opts_in() {
    let rt = rt();
    rt.execute_query("CREATE TABLE products (id INT, name TEXT)")
        .unwrap();
    assert!(!vcs(&rt).is_versioned("products").unwrap());
    rt.execute_query("ALTER TABLE products SET VERSIONED = true")
        .expect("ALTER SET VERSIONED = true");
    assert!(vcs(&rt).is_versioned("products").unwrap());
}

#[test]
fn alter_table_set_versioned_false_opts_out() {
    let rt = rt();
    rt.execute_query("CREATE TABLE products (id INT, name TEXT)")
        .unwrap();
    vcs(&rt).set_versioned("products", true).unwrap();
    rt.execute_query("ALTER TABLE products SET VERSIONED = false")
        .expect("ALTER SET VERSIONED = false");
    assert!(!vcs(&rt).is_versioned("products").unwrap());
}

#[test]
fn as_of_works_after_opt_in_retroactively() {
    // Classic retroactive flow: create, insert, commit, opt-in,
    // then AS OF the earlier commit. Must work — the xid is
    // pinned by the commit regardless of opt-in status at commit
    // time.
    let rt = rt();
    rt.execute_query("CREATE TABLE products (id INT, name TEXT)")
        .unwrap();
    let before = vcs(&rt)
        .commit(reddb::application::CreateCommitInput {
            connection_id: 1,
            message: "before opt-in".to_string(),
            author: reddb::application::Author {
                name: "t".to_string(),
                email: "t@x".to_string(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap();
    // Opt-in AFTER the commit — SQL path.
    rt.execute_query("ALTER TABLE products SET VERSIONED = true")
        .unwrap();

    let sql = format!("SELECT * FROM products AS OF COMMIT '{}'", before.hash);
    rt.execute_query(&sql)
        .expect("retroactive AS OF on opted-in table succeeds");
}

#[test]
fn as_of_works_after_opt_in() {
    let rt = rt();
    rt.execute_query("CREATE TABLE products (id INT, name TEXT)")
        .unwrap();
    vcs(&rt).set_versioned("products", true).unwrap();
    vcs(&rt)
        .commit(reddb::application::CreateCommitInput {
            connection_id: 1,
            message: "c1".to_string(),
            author: reddb::application::Author {
                name: "t".to_string(),
                email: "t@x".to_string(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap();

    rt.execute_query("SELECT * FROM products AS OF BRANCH 'main'")
        .expect("opt-in table accepts AS OF");
}
