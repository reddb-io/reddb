//! #911 — columnar storage activation, end-to-end.
//!
//! PRD #850 built the RDCC columnar engine (seal, granule index, bloom
//! skip, vectorized read) but left it dark: no production path activated
//! it. This test pins the activation wiring this slice adds:
//!
//!   1. `CREATE HYPERTABLE ... COLUMNAR` sets the contract's
//!      `analytical_storage.columnar = true`.
//!   2. `RedDBRuntime::seal_hypertable_chunks` — the production caller of
//!      `seal_chunk_with_config` — routes a columnar chunk's seal through
//!      the columnar arm, emitting an RDCC `ColumnBlock` and recording
//!      `ChunkMeta.columnar_page`.
//!   3. Read-back over that columnar chunk (the #856 column-block range
//!      scan) returns the ingested points.
//!   4. A hypertable created WITHOUT `COLUMNAR` still seals row-oriented:
//!      no `columnar_page`, no columnar block.

use reddb_server::{RedDBOptions, RedDBRuntime};

/// Points ingested in both cases. Timestamps are nanoseconds; with a 1h
/// chunk interval they all land in the single chunk that starts at 0.
const POINTS: &[(u64, i64)] = &[
    (1_000_000_000, 10),
    (2_000_000_000, 20),
    (3_000_000_000, 30),
    (4_000_000_000, 40),
    (5_000_000_000, 50),
];

fn seed(rt: &RedDBRuntime, ddl: &str, collection: &str) {
    rt.execute_query(ddl).expect("create hypertable");
    for (ts, value) in POINTS {
        rt.execute_query(&format!(
            "INSERT INTO {collection} (ts, value) VALUES ({ts}, {value})"
        ))
        .unwrap_or_else(|e| panic!("insert ({ts}, {value}): {e}"));
    }
}

#[test]
fn columnar_hypertable_seals_through_columnar_arm_and_reads_back() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed(
        &rt,
        "CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h' COLUMNAR",
        "cpu",
    );

    // Criterion 1: sealing routes through seal_chunk_with_config's
    // columnar arm — the single chunk seals columnar.
    let sealed = rt.seal_hypertable_chunks("cpu").expect("seal cpu");
    assert_eq!(sealed, 1, "the single chunk must seal columnar");

    // ... and the sealed chunk records a `ChunkMeta.columnar_page` (RDCC),
    // proving it did NOT take the row arm.
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        1,
        "columnar chunk must record ChunkMeta.columnar_page"
    );

    // Criterion 2: read-back over the columnar chunk via the #856
    // column-block range scan returns every ingested point.
    let got = rt
        .columnar_chunk_points("cpu", 0, 0, u64::MAX)
        .expect("columnar chunk decodes back to points");
    let want: Vec<(u64, f64)> = POINTS.iter().map(|(ts, v)| (*ts, *v as f64)).collect();
    assert_eq!(got, want, "read-back must return the ingested points");
}

#[test]
fn non_columnar_hypertable_seals_row_oriented() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed(
        &rt,
        "CREATE HYPERTABLE mem TIME_COLUMN ts CHUNK_INTERVAL '1h'",
        "mem",
    );

    // Criterion 3: without the flag the chunk seals row-oriented — no
    // columnar seal, no columnar_page, no columnar block to read.
    let sealed = rt.seal_hypertable_chunks("mem").expect("seal mem");
    assert_eq!(sealed, 0, "no chunk should seal columnar without the flag");
    assert_eq!(
        rt.columnar_chunk_count("mem"),
        0,
        "row-sealed chunk must NOT carry a columnar_page"
    );
    assert!(
        rt.columnar_chunk_points("mem", 0, 0, u64::MAX).is_none(),
        "row-sealed chunk exposes no columnar block"
    );
}
