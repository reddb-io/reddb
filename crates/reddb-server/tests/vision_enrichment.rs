//! #1275 — computer-vision enrichment over the CDC lane, end-to-end.
//!
//! A collection that declares a `VISION (...)` policy over an
//! image-reference field gets, asynchronously after commit:
//!   * structured component detections (`[{label, confidence, bbox}]`)
//!     written to a derived field that RQL can filter, and
//!   * an optional image-embedding vector queryable via `VECTOR SEARCH`.
//!
//! The pipeline rides the shared CDC enrichment consumer (#1272): rows are
//! excluded from vision queries until ready, and fetch/provider failures
//! retry with backoff and then dead-letter.
//!
//! These tests install a mock vision provider (no model download, no
//! network) and use a local image fixture referenced by a `file://` URI.

use std::sync::Arc;

use reddb_server::runtime::ai::cdc_enrichment::{CdcEnrichmentConsumer, EnrichmentKind};
use reddb_server::runtime::ai::vision::{
    install_local_vision_backend, LocalVisionBackend, VisionDetection, VisionRequest, VisionResult,
};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Deterministic mock vision provider. Returns fixed detections so the
/// RQL-filter assertions are stable, and a fixed-width embedding when the
/// policy requests one.
struct MockVisionProvider;

impl LocalVisionBackend for MockVisionProvider {
    fn analyze(&self, request: &VisionRequest) -> reddb_server::RedDBResult<VisionResult> {
        let detections = if request.want_detections {
            vec![
                VisionDetection {
                    label: "person".to_string(),
                    confidence: 0.91,
                    bbox: [0.10, 0.20, 0.30, 0.40],
                },
                VisionDetection {
                    label: "car".to_string(),
                    confidence: 0.72,
                    bbox: [0.50, 0.55, 0.20, 0.25],
                },
            ]
        } else {
            Vec::new()
        };
        let embedding = request.want_embedding.then(|| vec![0.25_f32; 8]);
        Ok(VisionResult {
            detections,
            embedding,
        })
    }
}

fn install_mock() {
    install_local_vision_backend(Arc::new(MockVisionProvider));
}

/// Write a tiny image fixture to a unique temp path and return a
/// `file://` URI referencing it.
fn write_fixture(tag: &str) -> (std::path::PathBuf, String) {
    let path = std::env::temp_dir().join(format!("reddb_vision_{tag}.png"));
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n mock image fixture").expect("write fixture");
    let uri = format!("file://{}", path.to_str().expect("utf8 path"));
    (path, uri)
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|e| panic!("query failed: {sql}\n  err: {e}"))
        .result
        .records
        .len()
}

#[test]
fn vision_detections_are_written_filterable_and_pending_excluded() {
    install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");

    rt.execute_query(
        "CREATE TABLE photos (id INT, photo TEXT, vision_detections JSON) \
         WITH (VISION (image_field = 'photo', outputs = ('detections'), \
         provider = 'local', model = 'mock-vision'))",
    )
    .expect("create photos with vision policy");

    let (fixture, uri) = write_fixture("primary");
    rt.execute_query(&format!(
        "INSERT INTO photos (id, photo) VALUES (1, '{uri}')"
    ))
    .expect("insert row");

    // Pending exclusion: before the consumer runs, a vision-component
    // filter must not surface the row — its detections are not ready.
    assert_eq!(
        row_count(
            &rt,
            "SELECT * FROM photos WHERE CONTAINS(vision_detections, 'person') = true"
        ),
        0,
        "row must be excluded from vision queries while pending"
    );

    // Drain the CDC stream: fetch image, run vision, attach detections.
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert!(stats.ingested >= 1, "the committed row must be ingested");
    assert!(stats.attached >= 1, "vision enrichment must attach");
    assert_eq!(
        consumer.pending_len(),
        0,
        "vision work must complete after the tick"
    );

    // Detections written + RQL-filterable: the row now matches a
    // component filter, and only for components actually detected.
    assert_eq!(
        row_count(
            &rt,
            "SELECT * FROM photos WHERE CONTAINS(vision_detections, 'person') = true"
        ),
        1,
        "row must surface once its 'person' detection is attached"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT * FROM photos WHERE CONTAINS(vision_detections, 'car') = true"
        ),
        1,
        "row must surface for its 'car' detection too"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT * FROM photos WHERE CONTAINS(vision_detections, 'bicycle') = true"
        ),
        0,
        "row must NOT match a component that was not detected"
    );

    // No self-trigger loop: the consumer's own detections write-back is an
    // UPDATE on the derived field (not the image_field), so re-ticking must
    // not re-enqueue the row, and the detections stay stable.
    let again = consumer.tick(&rt, 2_000).expect("second tick");
    assert_eq!(
        again.ingested, 0,
        "derived-field write-back must not re-enqueue vision"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT * FROM photos WHERE CONTAINS(vision_detections, 'person') = true"
        ),
        1,
        "re-ticking must not duplicate or drop detections"
    );

    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn vision_image_embedding_is_queryable_via_vector_search() {
    install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");

    rt.execute_query(
        "CREATE TABLE frames (id INT, photo TEXT, vision_detections JSON) \
         WITH (VISION (image_field = 'photo', outputs = ('detections', 'embedding'), \
         provider = 'local', model = 'mock-vision'))",
    )
    .expect("create frames with vision policy");

    let (fixture, uri) = write_fixture("embedding");
    rt.execute_query(&format!(
        "INSERT INTO frames (id, photo) VALUES (1, '{uri}')"
    ))
    .expect("insert row");

    // Before enrichment the vector does not exist — vector search is empty.
    assert_eq!(
        row_count(
            &rt,
            "VECTOR SEARCH frames SIMILAR TO [0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25] LIMIT 5",
        ),
        0,
        "no image embedding exists while the row is pending"
    );

    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 1_000).expect("tick");
    assert!(stats.attached >= 1, "vision enrichment must attach");

    // Optional image-embedding output: queryable via the existing vector
    // search once the policy requested it.
    let hits = row_count(
        &rt,
        "VECTOR SEARCH frames SIMILAR TO [0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.25] LIMIT 5",
    );
    assert!(
        hits >= 1,
        "image embedding must be queryable via VECTOR SEARCH"
    );

    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn vision_fetch_failure_retries_then_dead_letters() {
    install_mock();
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");

    rt.execute_query(
        "CREATE TABLE shots (id INT, photo TEXT, vision_detections JSON) \
         WITH (VISION (image_field = 'photo', outputs = ('detections'), \
         provider = 'local', model = 'mock-vision'))",
    )
    .expect("create shots with vision policy");

    // A reference that cannot be fetched — the image does not exist.
    rt.execute_query(
        "INSERT INTO shots (id, photo) VALUES (1, 'file:///nonexistent/reddb-vision-missing.png')",
    )
    .expect("insert row");

    let mut consumer = CdcEnrichmentConsumer::with_defaults();

    // First tick: ingest + first attempt fails → re-queued with backoff.
    let s0 = consumer.tick(&rt, 0).expect("tick 0");
    assert_eq!(s0.ingested, 1);
    assert_eq!(s0.attached, 0, "fetch must fail, nothing attached");
    assert_eq!(s0.retried, 1, "first failure retries");
    assert_eq!(
        consumer.pending_len(),
        1,
        "failed work stays pending across the retry budget"
    );
    assert!(consumer.dead_letters().is_empty());

    // Advance past the backoff windows; default budget is 3 attempts.
    let _ = consumer.tick(&rt, 1_000).expect("tick 1");
    let s2 = consumer.tick(&rt, 10_000).expect("tick 2");
    assert_eq!(s2.dead_lettered, 1, "exhausting retries dead-letters");
    assert_eq!(consumer.pending_len(), 0);

    let dead = consumer.dead_letters();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].collection, "shots");
    assert_eq!(dead[0].kind, EnrichmentKind::Vision);

    // Ops re-drive moves it back to pending with a fresh budget.
    assert_eq!(consumer.redrive(), 1);
    assert_eq!(consumer.pending_len(), 1);
    assert!(consumer.dead_letters().is_empty());
}
