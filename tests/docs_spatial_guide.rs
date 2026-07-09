//! `docs/guides/spatial-search.md` is executable documentation (issue #1938).
//!
//! The guide is the single authoritative page for the H3 spatial surface, so it
//! carries three kinds of claim that can rot independently:
//!
//!   * **SQL examples.** Every ` ```sql ` fence is extracted, in document order,
//!     and run against one fresh in-memory RedDB — the guide reads as a single
//!     coherent session, so it must execute as one.
//!   * **Quoted engine messages.** The coverage message, the zero-geo notice,
//!     and the `USING RTREE` removal error are quoted verbatim in the prose.
//!     Each is re-derived from a live runtime here and matched against the file,
//!     so a reworded message fails the docs lane instead of misleading a reader.
//!   * **The recognized/rejected shape matrix.** Each example shape in the two
//!     tables is fed through the real recognition seam via the `K of N` coverage
//!     count, so the tables cannot drift from behaviour.
//!
//! A ` ```sql ` fence whose first content line is `-- doctest:skip` is shown to
//! readers but not executed.

use reddb::runtime::RuntimeQueryResult;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};
use std::fs;
use std::path::PathBuf;

const SKIP_MARKER: &str = "-- doctest:skip";

fn guide_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is the umbrella crate root == repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("guides")
        .join("spatial-search.md")
}

fn guide() -> String {
    let path = guide_path();
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Extract the bodies of every ` ```sql ` fenced block, in document order.
fn sql_fences(markdown: &str) -> Vec<String> {
    let mut fences = Vec::new();
    let mut in_fence = false;
    let mut current = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if !in_fence {
            // Open only on a bare ```sql fence — the ```text / ```json blocks
            // are illustrative output, not statements.
            if trimmed == "```sql" {
                in_fence = true;
                current.clear();
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fences.push(std::mem::take(&mut current));
            in_fence = false;
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    fences
}

/// Split a fence into individual RQL statements: drop whole-line `--` comments,
/// split on `;`, and trim. The guide's SQL never embeds a `;` in a literal.
fn statements(fence: &str) -> Vec<String> {
    let without_comments: String = fence
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    without_comments
        .split(';')
        .map(|stmt| stmt.trim().to_string())
        .filter(|stmt| !stmt.is_empty())
        .collect()
}

fn is_skipped(fence: &str) -> bool {
    fence
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with(SKIP_MARKER))
}

fn message(res: &RuntimeQueryResult) -> String {
    match res
        .result
        .records
        .first()
        .and_then(|record| record.get("message"))
    {
        Some(Value::Text(message)) => message.to_string(),
        other => panic!("missing message row: {other:?}"),
    }
}

fn fresh() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("open in-memory RedDB")
}

/// Assert the guide quotes `text` verbatim somewhere in its prose.
fn assert_guide_quotes(text: &str) {
    let markdown = guide();
    assert!(
        markdown.contains(text),
        "docs/guides/spatial-search.md does not quote the engine's own string:\n  {text}\n\
         The engine changed and the guide did not. Update the guide."
    );
}

/// Build a one-document collection around `body` and report how the H3 index
/// build over `column` counted it — the recognition seam, observed through the
/// coverage message.
fn indexed_count_for(body: &str, column: &str) -> usize {
    let rt = fresh();
    rt.execute_query("CREATE DOCUMENT probe").expect("create");
    rt.execute_query(&format!("INSERT INTO probe DOCUMENT VALUES ({body})"))
        .unwrap_or_else(|err| panic!("insert {body}: {err:?}"));
    let res = rt
        .execute_query(&format!(
            "CREATE INDEX idx_probe ON probe ({column}) USING H3"
        ))
        .expect("create index");
    let msg = message(&res);
    // "... using H3 (K of 1 entities indexed...)"
    let marker = " using H3 (";
    let tail = &msg[msg.find(marker).expect("coverage detail") + marker.len()..];
    let k: usize = tail
        .split_whitespace()
        .next()
        .expect("K")
        .parse()
        .unwrap_or_else(|_| panic!("unparsable coverage in {msg}"));
    k
}

// ── The SQL examples run ─────────────────────────────────────────────────────

#[test]
fn every_sql_example_in_the_guide_executes() {
    let markdown = guide();
    let fences = sql_fences(&markdown);
    assert!(
        fences.len() >= 8,
        "expected the guide to carry its worked examples, found {} sql fences",
        fences.len()
    );

    let rt = fresh();
    let mut executed = 0usize;
    let mut produced_row = false;
    for fence in &fences {
        if is_skipped(fence) {
            continue;
        }
        for stmt in statements(fence) {
            let result = rt.execute_query(&stmt).unwrap_or_else(|err| {
                panic!("guide statement is not runnable:\n  {stmt}\n-> {err:?}")
            });
            executed += 1;
            if !result.result.records.is_empty() {
                produced_row = true;
            }
        }
    }
    assert!(executed >= 15, "only {executed} runnable statements");
    assert!(produced_row, "no statement in the guide returned a row");
}

// ── The quoted engine messages are the engine's ──────────────────────────────

#[test]
fn guide_quotes_the_real_coverage_messages() {
    let rt = fresh();
    rt.execute_query("CREATE TABLE stores (id INT, name TEXT, location GEOPOINT)")
        .unwrap();
    for (id, name, loc) in [
        (1, "Louvre", "48.8606,2.3376"),
        (2, "Eiffel Tower", "48.8584,2.2945"),
        (3, "Sacre-Coeur", "48.8867,2.3431"),
        (4, "Gare de Lyon", "48.8443,2.3743"),
    ] {
        rt.execute_query(&format!(
            "INSERT INTO stores (id, name, location) VALUES ({id}, '{name}', '{loc}')"
        ))
        .unwrap();
    }
    let res = rt
        .execute_query("CREATE INDEX idx_stores_loc ON stores (location) USING H3")
        .unwrap();
    assert_guide_quotes(&message(&res));

    // Zero coverage over a non-empty collection elaborates with the shape hint.
    let rt = fresh();
    rt.execute_query("CREATE DOCUMENT sensors").unwrap();
    rt.execute_query(
        r#"INSERT INTO sensors DOCUMENT VALUES
             ({"id":1,"spot":"38.76,-77.15"}),
             ({"id":2,"spot":{"type":"Point","coordinates":[-77.15,38.76]}}),
             ({"id":3,"spot":{"lat":38.76}})"#,
    )
    .unwrap();
    let res = rt
        .execute_query("CREATE INDEX idx_sensors_spot ON sensors (spot) USING H3")
        .unwrap();
    assert_guide_quotes(&message(&res));
    assert!(
        message(&res).contains(reddb::geo::RECOGNIZED_GEO_SHAPES),
        "the coverage hint no longer names the recognized shapes"
    );
}

#[test]
fn guide_quotes_the_real_zero_geo_notice() {
    let rt = fresh();
    rt.execute_query("CREATE DOCUMENT sensors").unwrap();
    rt.execute_query(
        r#"INSERT INTO sensors DOCUMENT VALUES
             ({"id":1,"spot":"38.76,-77.15"}),
             ({"id":2,"spot":{"type":"Point","coordinates":[-77.15,38.76]}}),
             ({"id":3,"spot":{"lat":38.76}})"#,
    )
    .unwrap();
    let res = rt
        .execute_query("SEARCH SPATIAL RADIUS 38.76 -77.15 10.0 COLLECTION sensors COLUMN spot")
        .unwrap();
    assert!(res.result.records.is_empty());
    let notice = res.notice.expect("shape mismatch must carry a notice");
    assert_guide_quotes(&notice);

    // ...and the guide's two "absent" cases really are absent.
    let empty = fresh();
    empty.execute_query("CREATE DOCUMENT sensors").unwrap();
    let res = empty
        .execute_query("SEARCH SPATIAL RADIUS 38.76 -77.15 10.0 COLLECTION sensors COLUMN spot")
        .unwrap();
    assert_eq!(res.notice, None, "an empty collection must not be nagged");

    let miss = fresh();
    miss.execute_query("CREATE DOCUMENT sensors").unwrap();
    miss.execute_query(
        r#"INSERT INTO sensors DOCUMENT VALUES ({"spot":{"lat":40.7,"lon":-74.0}})"#,
    )
    .unwrap();
    let res = miss
        .execute_query("SEARCH SPATIAL RADIUS 38.76 -77.15 1.0 COLLECTION sensors COLUMN spot")
        .unwrap();
    assert_eq!(res.notice, None, "an out-of-range hit is not a mismatch");
}

#[test]
fn guide_quotes_the_real_rtree_removal_error() {
    let rt = fresh();
    rt.execute_query("CREATE TABLE stores (id INT, location GEOPOINT)")
        .unwrap();
    let err = rt
        .execute_query("CREATE INDEX idx_loc ON stores (location) USING RTREE")
        .expect_err("USING RTREE must be rejected");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("USING RTREE was removed"),
        "unexpected rejection: {rendered}"
    );
    // The guide reproduces the didactic message; pin its load-bearing halves.
    for fragment in [
        "USING RTREE was removed: the in-RAM R-tree indexed nothing and served no queries.",
        "Use USING H3 — same SEARCH SPATIAL surface, disk-resident, maintained on every write.",
    ] {
        assert!(
            rendered.contains(fragment),
            "engine message lost {fragment:?}: {rendered}"
        );
        assert_guide_quotes(fragment);
    }
}

// ── The shape matrix matches the recognition seam ────────────────────────────

#[test]
fn guide_recognized_shapes_table_matches_the_seam() {
    // Every example is quoted in the guide's "Recognized shapes" table and is
    // accepted by the seam.
    for body in [
        r#"{"lat":38.76,"lon":-77.15}"#,
        r#"{"latitude":38.76,"longitude":-77.15}"#,
        r#"{"lat":38.76,"lng":-77.15}"#,
        r#"{"lat":39,"lon":-77}"#,
        r#"{"lat":38.76,"lon":-77.15,"accuracy":5}"#,
    ] {
        assert_guide_quotes(body);
        assert_eq!(
            indexed_count_for(&format!(r#"{{"spot":{body}}}"#), "spot"),
            1,
            "guide lists {body} as recognized, the seam rejects it"
        );
    }

    // A GEOPOINT column is the row-table entry point.
    let rt = fresh();
    rt.execute_query("CREATE TABLE places (id INT, location GEOPOINT)")
        .unwrap();
    rt.execute_query("INSERT INTO places (id, location) VALUES (1, '48.8606,2.3376')")
        .unwrap();
    let res = rt
        .execute_query("CREATE INDEX idx_places ON places (location) USING H3")
        .unwrap();
    assert!(message(&res).contains("1 of 1 entities indexed"));
}

#[test]
fn guide_rejected_shapes_table_matches_the_seam() {
    // Object-shaped rejects, quoted in the guide's "Rejected shapes" table.
    for body in [
        r#"{"lat":"38.76","lon":"-77.15"}"#,
        r#"{"type":"Point","coordinates":[-77.15,38.76]}"#,
        r#"{"lat":38.76}"#,
        r#"{"lat":38.76,"lon":null}"#,
        r#"{"lat":91,"lon":0}"#,
        r#"{"lat":0,"lon":-181}"#,
    ] {
        assert_guide_quotes(body);
        assert_eq!(
            indexed_count_for(&format!(r#"{{"spot":{body}}}"#), "spot"),
            0,
            "guide lists {body} as rejected, the seam accepts it"
        );
    }

    // Non-object rejects.
    for body in [r#""38.76,-77.15""#, r#"[38.76,-77.15]"#] {
        assert_eq!(
            indexed_count_for(&format!(r#"{{"spot":{body}}}"#), "spot"),
            0,
            "guide lists {body} as rejected, the seam accepts it"
        );
    }

    // Key matching is case-sensitive, as the guide states.
    assert_eq!(
        indexed_count_for(r#"{"spot":{"Lat":38.76,"Lon":-77.15}}"#, "spot"),
        0,
        "guide states key matching is case-sensitive"
    );
}

// ── The resolution guidance matches the grid ─────────────────────────────────

#[test]
fn guide_resolution_table_matches_h3_edge_lengths() {
    // (resolution, the "~X km" edge the guide prints)
    const ROWS: &[(u8, f64)] = &[(5, 9.9), (7, 1.4), (9, 0.20), (11, 0.029), (13, 0.0041)];
    for &(res, shown) in ROWS {
        let actual = reddb::geo::h3::edge_length_km(res);
        let tolerance = shown * 0.06;
        assert!(
            (actual - shown).abs() <= tolerance,
            "guide prints ~{shown} km for resolution {res}, the grid says {actual}"
        );
    }
    assert_guide_quotes("| `9` (default) | ~0.20 km |");

    // The default the guide names is the default the parser applies.
    let rt = fresh();
    rt.execute_query("CREATE TABLE places (id INT, location GEOPOINT)")
        .unwrap();
    rt.execute_query("INSERT INTO places (id, location) VALUES (1, '48.8606,2.3376')")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_default ON places (location) USING H3")
        .expect("default resolution");
    rt.execute_query("CREATE INDEX idx_explicit ON places (location) USING H3 (9)")
        .expect("explicit resolution 9");
    rt.execute_query("CREATE INDEX idx_bad ON places (location) USING H3 (16)")
        .expect_err("resolution 16 is out of range");
}

// ── Discoverability ──────────────────────────────────────────────────────────

#[test]
fn guide_is_cross_linked_both_ways() {
    let docs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs");
    let link = "/guides/spatial-search.md";

    let sidebar = fs::read_to_string(docs.join("_sidebar.md")).expect("read _sidebar.md");
    assert!(
        sidebar.contains(link),
        "docs/_sidebar.md must link the spatial guide"
    );

    // The guide is reachable from the Documents walkthrough and the index/DDL
    // docs, and links back to both (issue #1938 acceptance).
    for (page, back_link) in [
        ("data-models/documents.md", "/data-models/documents.md"),
        ("query/create-index.md", "/query/create-index.md"),
        ("query/spatial-search.md", "/query/spatial-search.md"),
    ] {
        let contents =
            fs::read_to_string(docs.join(page)).unwrap_or_else(|e| panic!("{page}: {e}"));
        assert!(contents.contains(link), "docs/{page} must link the guide");
        assert!(
            guide().contains(back_link),
            "the guide must link back to docs/{page}"
        );
    }
}

// ── The worked example prints what the engine prints ─────────────────────────

/// Seed the guide's `stores` table, exactly as its first fence does.
fn seeded_stores() -> RedDBRuntime {
    let rt = fresh();
    rt.execute_query("CREATE TABLE stores (id INT, name TEXT, location GEOPOINT)")
        .unwrap();
    for (id, name, loc) in [
        (1, "Louvre", "48.8606,2.3376"),
        (2, "Eiffel Tower", "48.8584,2.2945"),
        (3, "Sacre-Coeur", "48.8867,2.3431"),
        (4, "Gare de Lyon", "48.8443,2.3743"),
    ] {
        rt.execute_query(&format!(
            "INSERT INTO stores (id, name, location) VALUES ({id}, '{name}', '{loc}')"
        ))
        .unwrap();
    }
    rt
}

fn rows(rt: &RedDBRuntime, query: &str) -> Vec<(u64, Option<f64>)> {
    let res = rt
        .execute_query(query)
        .unwrap_or_else(|e| panic!("{query}: {e:?}"));
    res.result
        .records
        .iter()
        .map(|record| {
            let id = match record.get("entity_id") {
                Some(Value::UnsignedInteger(id)) => *id,
                other => panic!("entity_id: {other:?}"),
            };
            let dist = match record.get("distance_km") {
                Some(Value::Float(d)) => Some(*d),
                None => None,
                other => panic!("distance_km: {other:?}"),
            };
            (id, dist)
        })
        .collect()
}

#[test]
fn guide_worked_example_prints_the_real_output() {
    let rt = seeded_stores();
    rt.execute_query("CREATE INDEX idx_stores_loc ON stores (location) USING H3")
        .unwrap();

    let radius = rows(
        &rt,
        "SEARCH SPATIAL RADIUS 48.8566 2.3522 3.0 COLLECTION stores COLUMN location",
    );
    // The guide prints these ids and these distances, verbatim.
    for (id, dist) in &radius {
        assert_guide_quotes(&id.to_string());
        assert_guide_quotes(&dist.expect("RADIUS returns a distance").to_string());
    }
    assert_eq!(radius.len(), 2, "the guide shows two hits within 3 km");
    // ...and the two the guide names as absent really are.
    assert!(!radius.iter().any(|(id, _)| *id == 1027 || *id == 1028));

    // NEAREST K 2 is the same two rows, as the guide claims.
    let nearest = rows(
        &rt,
        "SEARCH SPATIAL NEAREST 48.8566 2.3522 K 2 COLLECTION stores COLUMN location",
    );
    assert_eq!(nearest, radius);

    // BBOX carries no distance and comes back in scan order.
    let bbox = rows(
        &rt,
        "SEARCH SPATIAL BBOX 48.85 2.30 48.89 2.40 COLLECTION stores COLUMN location LIMIT 10",
    );
    assert!(bbox.iter().all(|(_, dist)| dist.is_none()));
    for (id, _) in &bbox {
        assert_guide_quotes(&id.to_string());
    }
}

#[test]
fn guide_promise_that_the_index_is_a_pure_optimization_holds() {
    const QUERIES: &[&str] = &[
        "SEARCH SPATIAL RADIUS 48.8566 2.3522 3.0 COLLECTION stores COLUMN location",
        "SEARCH SPATIAL NEAREST 48.8566 2.3522 K 3 COLLECTION stores COLUMN location",
        "SEARCH SPATIAL BBOX 48.85 2.30 48.89 2.40 COLLECTION stores COLUMN location",
    ];

    let scan = seeded_stores();
    let indexed = seeded_stores();
    indexed
        .execute_query("CREATE INDEX idx_stores_loc ON stores (location) USING H3")
        .unwrap();

    for query in QUERIES {
        let without = rows(&scan, query);
        let with = rows(&indexed, query);
        // Same rows, same order, bit-for-bit identical distances.
        assert_eq!(without.len(), with.len(), "{query}: row count diverged");
        for (a, b) in without.iter().zip(with.iter()) {
            assert_eq!(a.0, b.0, "{query}: entity_id or ordering diverged");
            assert_eq!(
                a.1.map(f64::to_bits),
                b.1.map(f64::to_bits),
                "{query}: distance diverged"
            );
        }
    }
}
