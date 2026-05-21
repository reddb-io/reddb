//! Reproduction for bug #4 ("the big one"): typed table column names not
//! persisted across a file-backed close + reopen once the row count crosses
//! a snapshot/WAL boundary (~500 rows).
//!
//! Externally observable assertions only — we never touch private internals.
//! We assert on what `SELECT` actually returns: the projected column names,
//! the `SELECT *` column set, and the row contents of a filtered query.

use reddb_server::{RedDBOptions, RedDBRuntime};

const ROWS: usize = 2500;
const CHUNK: usize = 300;

fn insert_chunks(rt: &RedDBRuntime) {
    let mut inserted = 0usize;
    while inserted < ROWS {
        let end = (inserted + CHUNK).min(ROWS);
        let values: Vec<String> = (inserted..end)
            .map(|i| {
                // Make exactly one row carry word='ring' so the filtered
                // SELECT below has a deterministic single-row expectation.
                let word = if i == 7 { "ring" } else { "other" };
                format!("('chapter{}', '{}', {})", i % 10, word, i as i64)
            })
            .collect();
        let sql = format!(
            "INSERT INTO chapter_words (chapter, word, freq) VALUES {}",
            values.join(", ")
        );
        rt.execute_query(&sql)
            .unwrap_or_else(|e| panic!("insert chunk [{inserted}..{end}) failed: {e}"));
        inserted = end;
    }
}

#[test]
fn issue4_typed_column_names_survive_persistent_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("chapter_words.rdb");

    // ---- First connection: create + bulk insert ----
    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("runtime boots");
        rt.execute_query(
            "CREATE TABLE chapter_words (chapter TEXT, word TEXT, freq INTEGER)",
        )
        .expect("create table");

        insert_chunks(&rt);

        // Same-connection sanity: named columns must work right after insert.
        let same = rt
            .execute_query("SELECT chapter, freq FROM chapter_words WHERE word = 'ring'")
            .expect("same-connection select");
        assert_eq!(
            same.result.columns,
            vec!["chapter".to_string(), "freq".to_string()],
            "same-connection projection must carry declared names"
        );
        assert_eq!(
            same.result.records.len(),
            1,
            "same-connection filtered query must find the one word='ring' row"
        );

        // Drop the runtime to release the pager + file lock so the same
        // process can reopen the persistent file (see impl_core.rs comment
        // about the maintenance thread holding only a Weak ref).
        drop(rt);
    }

    // ---- Second connection: reopen the SAME file ----
    let rt2 =
        RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("runtime reopens");

    let reopened = rt2
        .execute_query("SELECT chapter, freq FROM chapter_words WHERE word = 'ring'")
        .expect("reopened select executes");

    let star = rt2
        .execute_query("SELECT * FROM chapter_words")
        .expect("reopened select-star executes");

    eprintln!("REOPEN projection columns = {:?}", reopened.result.columns);
    eprintln!("REOPEN projection rows     = {}", reopened.result.records.len());
    eprintln!("REOPEN select-* columns    = {:?}", star.result.columns);

    // The bug report: after reopen these degrade to c0/c1/c2 and the
    // filtered query returns no rows.
    assert_eq!(
        reopened.result.columns,
        vec!["chapter".to_string(), "freq".to_string()],
        "reopened projection lost declared column names (got {:?})",
        reopened.result.columns
    );
    assert_eq!(
        reopened.result.records.len(),
        1,
        "reopened filtered query lost rows (word='ring' should match exactly one)"
    );

    assert!(
        star.result.columns.contains(&"chapter".to_string())
            && star.result.columns.contains(&"word".to_string())
            && star.result.columns.contains(&"freq".to_string()),
        "reopened SELECT * lost declared column names (got {:?})",
        star.result.columns
    );
}
