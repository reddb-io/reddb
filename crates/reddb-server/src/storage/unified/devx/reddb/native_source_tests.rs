use super::*;

fn paged_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(crate::storage::StorageDeployPreset::Serverless.selection())
        .expect("paged profile")
}

fn seed_runtime(options: RedDBOptions) -> crate::RedDBRuntime {
    let runtime = crate::RedDBRuntime::with_options(options).expect("runtime");
    runtime
        .execute_query("CREATE TABLE cold_rows (id INT,payload TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO cold_rows (id,payload) VALUES (1,'alpha beta')")
        .expect("row");
    runtime.checkpoint().expect("publish artifacts");
    runtime
}

fn descriptors(db: &RedDB) -> Vec<NativeVectorArtifactPageSummary> {
    let header = db.store.physical_file_header().expect("header");
    db.store
        .read_native_vector_artifact_store(header.vector_artifact_page)
        .expect("descriptors")
}

fn assert_artifact_matches_source(db: &RedDB) {
    let header = db.store.physical_file_header().expect("published header");
    for (summary, expected) in db.native_vector_artifact_records(&["cold_rows".into()]) {
        let (_, actual) = db
            .store
            .read_native_vector_artifact_blob(
                header.vector_artifact_page,
                &summary.collection,
                Some(&summary.artifact_kind),
            )
            .expect("read frame")
            .expect("published frame");
        assert_eq!(actual, expected);
    }
}

#[test]
fn prepared_native_artifacts_reject_a_later_source_generation() {
    let directory = tempfile::tempdir().expect("temporary native store");
    let runtime = seed_runtime(paged_options(&directory.path().join("prepared.rdb")));
    runtime
        .execute_query("UPDATE cold_rows SET payload='beta gamma'")
        .expect("first edit");
    let db = runtime.db();
    let mut prepared = db.prepare_native_artifact_publication(&descriptors(&db));
    assert!(prepared
        .records
        .iter()
        .any(|(summary, _)| summary.collection == "cold_rows"));
    runtime
        .execute_query("UPDATE cold_rows SET payload='delta epsilon'")
        .expect("later edit");
    db.validate_native_artifact_publication(&mut prepared, &descriptors(&db));
    assert!(!prepared.versions.contains_key("cold_rows"));
    assert!(prepared
        .records
        .iter()
        .all(|(summary, _)| summary.collection != "cold_rows"));
    assert!(prepared
        .retained
        .iter()
        .all(|summary| summary.collection != "cold_rows"));
    assert!(prepared
        .summaries
        .iter()
        .all(|summary| summary.collection != "cold_rows"));
    runtime
        .checkpoint()
        .expect("materialize current generation");
    assert_artifact_matches_source(&db);
}

#[test]
fn prepared_native_retention_rejects_a_replaced_page_reference() {
    let directory = tempfile::tempdir().expect("temporary native store");
    let runtime = seed_runtime(paged_options(&directory.path().join("retained.rdb")));
    let db = runtime.db();
    let existing = descriptors(&db);
    let mut prepared = db.prepare_native_artifact_publication(&existing);
    assert!(prepared
        .retained
        .iter()
        .any(|page| page.collection == "cold_rows"));
    let mut replaced = existing;
    for page in &mut replaced {
        if page.collection == "cold_rows" {
            page.checksum ^= 1;
        }
    }
    db.validate_native_artifact_publication(&mut prepared, &replaced);
    assert!(!prepared.versions.contains_key("cold_rows"));
    assert!(prepared
        .retained
        .iter()
        .all(|page| page.collection != "cold_rows"));
}

#[test]
fn overlapping_preparation_does_not_wait_under_publication() {
    let directory = tempfile::tempdir().expect("temporary native store");
    let runtime = seed_runtime(paged_options(&directory.path().join("admission.rdb")));
    runtime
        .execute_query("UPDATE cold_rows SET payload='changed source'")
        .expect("edit");
    let db = Arc::clone(&runtime.db());
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let owner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let _admission = db.native_artifact_preparation.lock();
            entered_tx.send(()).expect("admission observer");
            release
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("release preparation");
        })
    };
    entered
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("admitted preparation");
    let (completed_tx, completed) = std::sync::mpsc::channel();
    let publisher = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let _publication = db.store.vault_publication_lock.lock();
            let result = db.flush_local_only().map_err(|error| error.to_string());
            completed_tx.send(result).expect("publication observer");
        })
    };
    let result = completed.recv_timeout(std::time::Duration::from_secs(5));
    release_tx.send(()).expect("release admission");
    owner.join().expect("admission owner");
    publisher.join().expect("overlapping publisher");
    result
        .expect("publication must not wait for preparation")
        .expect("publication succeeds");
    assert!(descriptors(&db)
        .iter()
        .all(|page| page.collection != "cold_rows"));
    runtime.checkpoint().expect("materialize after admission");
    assert_artifact_matches_source(&db);
}

#[test]
fn cold_concurrent_checkpoints_wait_before_publication_and_retain_caches() {
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::Layer;

    struct Message(bool);
    impl Visit for Message {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message"
                && format!("{value:?}") == "native artifact snapshots started"
            {
                self.0 = true;
            }
        }
    }
    struct Pause {
        reached: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl<S: Subscriber> Layer<S> for Pause {
        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() != "reddb::checkpoint" {
                return;
            }
            let mut message = Message(false);
            event.record(&mut message);
            if message.0 {
                self.reached.send(()).expect("preparation observer");
                self.release
                    .lock()
                    .expect("pause receiver")
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("release preparation");
            }
        }
    }

    let directory = tempfile::tempdir().expect("temporary native store");
    let options = paged_options(&directory.path().join("cold-concurrent.rdb"));
    let runtime = seed_runtime(options.clone());
    runtime.execute_query("INSERT INTO cold_rows (id,payload) VALUES (2,'alpha beta'),(3,'alpha beta'),(4,'alpha beta'),(5,'alpha beta'),(6,'alpha beta')").expect("six source rows");
    runtime.checkpoint().expect("publish source rows");
    drop(runtime);
    let db = Arc::new(RedDB::open_with_options(&options).expect("cold engine"));
    assert!(
        db.native_artifact_versions.lock().is_empty(),
        "cold in-memory proofs"
    );
    let canonical = |mut pages: Vec<NativeVectorArtifactPageSummary>| {
        pages.sort_by(|left, right| {
            (&left.collection, &left.artifact_kind).cmp(&(&right.collection, &right.artifact_kind))
        });
        pages
    };
    let before = canonical(descriptors(&db));
    assert!(!before.is_empty());
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let first = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let subscriber = tracing_subscriber::registry().with(Pause {
                reached: entered_tx,
                release: std::sync::Mutex::new(release),
            });
            tracing::subscriber::with_default(subscriber, || {
                db.flush_local_only().map_err(|error| error.to_string())
            })
        })
    };
    entered
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("first preparation");
    let (completed_tx, completed) = std::sync::mpsc::channel();
    let second = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            completed_tx
                .send(db.flush_local_only().map_err(|error| error.to_string()))
                .expect("second checkpoint observer");
        })
    };
    let waiting = matches!(
        completed.recv_timeout(std::time::Duration::from_secs(5)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
    let publication_available = db.store.vault_publication_lock.try_lock().is_some();
    let during = canonical(descriptors(&db));
    release_tx.send(()).expect("release first checkpoint");
    first
        .join()
        .expect("first checkpoint worker")
        .expect("first checkpoint");
    second.join().expect("second checkpoint worker");
    if waiting {
        completed
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("second checkpoint completion")
            .expect("second checkpoint");
    }
    assert!(
        waiting,
        "ordinary checkpoint must wait outside publication for the admitted preparation"
    );
    assert!(
        publication_available,
        "waiting for preparation must not hold row/WAL publication"
    );
    assert_eq!(
        during, before,
        "overlap must not evict unchanged cold caches"
    );
    assert_eq!(
        canonical(descriptors(&db)),
        before,
        "both checkpoints must retain the same physical references"
    );
    assert_eq!(
        db.store
            .get_collection("cold_rows")
            .expect("source rows")
            .query_all(|_| true)
            .len(),
        6
    );
    assert_artifact_matches_source(&db);
}

#[test]
fn native_artifacts_reuse_after_clean_cold_reopen() {
    let directory = tempfile::tempdir().expect("temporary native store");
    let path = directory.path().join("cold-reuse.rdb");
    let options = paged_options(&path);
    drop(seed_runtime(options.clone()));
    for _ in 0..3 {
        // Opening the engine alone leaves the source unchanged. Runtime boot
        // intentionally writes red.system and is covered by the next test.
        let db = RedDB::open_with_options(&options).expect("cold engine reopen");
        let existing = descriptors(&db);
        assert!(existing.iter().any(|page| page.collection == "cold_rows"));
        let publication = db.prepare_native_artifact_publication(&existing);
        assert!(
            publication.records.is_empty(),
            "unchanged sources must not rebuild"
        );
        assert_eq!(publication.retained.len(), existing.len());
        assert_artifact_matches_source(&db);
        db.flush_local_only().expect("publish reused sources");
    }
}

#[test]
fn native_artifacts_runtime_reopen_rebuilds_only_changed_boot_configuration() {
    let directory = tempfile::tempdir().expect("temporary runtime store");
    let options = paged_options(&directory.path().join("runtime-reuse.rdb"));
    drop(seed_runtime(options.clone()));
    let runtime = crate::RedDBRuntime::with_options(options).expect("runtime reopen");
    let db = runtime.db();
    let existing = descriptors(&db);
    let header = db.store.physical_file_header().expect("header");
    let proofs = db
        .store
        .read_native_artifact_sources(header.vector_artifact_page)
        .expect("proofs");
    let configuration = proofs
        .iter()
        .find(|proof| proof.collection == "red_config")
        .expect("configuration source");
    assert_ne!(
        configuration.source_digest,
        RedDB::native_artifact_source_digest(
            "red_config",
            &db.store
                .get_collection("red_config")
                .expect("boot configuration"),
        ),
        "runtime boot writes pid, started_at and memory availability"
    );
    let publication = db.prepare_native_artifact_publication(&existing);
    let changed: BTreeSet<_> = publication
        .records
        .iter()
        .map(|(summary, _)| summary.collection.as_str())
        .collect();
    assert_eq!(changed, BTreeSet::from(["red_config"]));
    assert_eq!(
        publication.retained.len(),
        existing
            .iter()
            .filter(|page| page.collection != "red_config")
            .count()
    );
    assert_artifact_matches_source(&db);
}

#[test]
fn native_artifacts_reject_stale_sources_after_crash() {
    const CHILD_PATH: &str = "REDDB_TEST_NATIVE_SOURCE_CRASH_PATH";
    const CHILD_PHASE: &str = "REDDB_TEST_NATIVE_SOURCE_CRASH_PHASE";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let runtime = seed_runtime(paged_options(Path::new(&path)));
        let phase = std::env::var(CHILD_PHASE).expect("crash phase");
        if phase.starts_with("compressed-") {
            for batch in 0..16 {
                let rows = (10 + batch * 128..10 + (batch + 1) * 128)
                    .map(|id| format!("({id},'alpha beta gamma delta epsilon theta shared text')"))
                    .collect::<Vec<_>>()
                    .join(",");
                runtime
                    .execute_query(&format!("INSERT INTO cold_rows (id,payload) VALUES {rows}"))
                    .expect("indexed corpus");
            }
            runtime.checkpoint().expect("publish compressed source");
            let db = runtime.db();
            let reference = descriptors(&db)
                .into_iter()
                .find(|page| page.collection == "cold_rows")
                .expect("compressed reference");
            let page = db
                .store
                .pager()
                .expect("pager")
                .read_page(reference.root_page)
                .expect("compressed root");
            assert!(
                reddb_file::native_blob_compression::native_blob_compression_header(
                    &page.as_bytes()[crate::storage::engine::HEADER_SIZE..]
                )
                .expect("codec header")
                .is_some()
            );
        }
        if phase == "active" || phase == "aborted" {
            runtime.execute_query("BEGIN").expect("transaction");
            runtime
                .execute_query("INSERT INTO cold_rows (id,payload) VALUES (2,'uncommitted')")
                .expect("unfinished row");
            runtime
                .checkpoint()
                .expect("checkpoint excludes unfinished primary row");
            if phase == "aborted" {
                runtime.execute_query("ROLLBACK").expect("abort");
            }
        } else {
            runtime
                .execute_query("UPDATE cold_rows SET payload='omega delta' WHERE id=1")
                .expect("acknowledged update");
            if phase.ends_with("primary") {
                // Crash between primary durability and native publication.
                runtime.db().store.persist().expect("durable primary only");
            } else {
                assert!(
                    std::fs::metadata(reddb_file::layout::unified_wal_path(Path::new(&path)))
                        .expect("recovery WAL")
                        .len()
                        > 8
                );
            }
        }
        std::process::exit(0); // No destructor may repair this fixture.
    }
    for phase in [
        "wal",
        "primary",
        "active",
        "aborted",
        "compressed-wal",
        "compressed-primary",
    ] {
        let directory = tempfile::tempdir().expect("crash fixture");
        let path = directory.path().join("crash.rdb");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "storage::unified::devx::reddb::native_source_tests::native_artifacts_reject_stale_sources_after_crash", "--nocapture"])
            .env(CHILD_PATH, &path).env(CHILD_PHASE, phase).status().expect("crash child");
        assert!(status.success(), "crash fixture failed: {phase}");
        let runtime = crate::RedDBRuntime::with_options(paged_options(&path)).expect("recovery");
        let result = runtime
            .execute_query("SELECT payload FROM cold_rows WHERE id=1")
            .expect("recovered value");
        let expected = if phase == "wal" || phase.ends_with("primary") || phase == "compressed-wal"
        {
            "omega delta"
        } else {
            "alpha beta"
        };
        assert_eq!(
            result.result.records[0]
                .get("payload")
                .expect("payload")
                .as_text(),
            Some(expected),
            "{phase}"
        );
        assert!(runtime
            .execute_query("SELECT id FROM cold_rows WHERE id=2")
            .expect("uncommitted row check")
            .result
            .records
            .is_empty());
        let db = runtime.db();
        let publication = db.prepare_native_artifact_publication(&descriptors(&db));
        assert!(
            publication
                .records
                .iter()
                .any(|(summary, _)| summary.collection == "cold_rows"),
            "stale artifacts must rebuild: {phase}"
        );
        runtime.checkpoint().expect("publish recovered artifacts");
        assert_artifact_matches_source(&db);
        if phase.starts_with("compressed-") {
            drop(db);
            drop(runtime);
            let cold = RedDB::open_with_options(&paged_options(&path)).expect("cold packed source");
            assert!(cold
                .prepare_native_artifact_publication(&descriptors(&cold))
                .records
                .iter()
                .all(|(summary, _)| summary.collection != "cold_rows"));
        }
    }
}

#[test]
fn native_artifacts_cold_admission_checks_payloads_and_legacy_fallback() {
    let directory = tempfile::tempdir().expect("corruption fixture");
    let runtime = seed_runtime(paged_options(&directory.path().join("corrupt.rdb")));
    let db = runtime.db();
    let existing = descriptors(&db);
    let reference = existing
        .iter()
        .find(|page| page.collection == "cold_rows")
        .expect("row artifact");
    let pager = db.store.pager().expect("pager");
    let original = pager.read_page(reference.root_page).expect("artifact page");
    let mut corrupt = original.clone();
    corrupt.as_bytes_mut()
        [crate::storage::engine::HEADER_SIZE + reddb_file::NATIVE_BLOB_PAGE_HEADER_BYTES] ^= 1;
    pager
        .write_page(reference.root_page, corrupt)
        .expect("valid page with wrong payload");
    db.native_artifact_versions.lock().clear();
    assert!(db
        .prepare_native_artifact_publication(&existing)
        .records
        .iter()
        .any(|(summary, _)| summary.collection == "cold_rows"));
    pager
        .write_page(reference.root_page, original)
        .expect("restore fixture payload");
    let mut stale = existing.clone();
    stale
        .iter_mut()
        .find(|page| page.collection == "cold_rows")
        .expect("reference")
        .checksum ^= 1;
    assert!(db
        .prepare_native_artifact_publication(&stale)
        .records
        .iter()
        .any(|(summary, _)| summary.collection == "cold_rows"));

    let mut header = db.store.physical_file_header().expect("header");
    let (page, checksum, _) = db
        .store
        .write_native_vector_artifact_store_retaining(
            &[],
            &existing,
            Some(header.vector_artifact_page),
        )
        .expect("legacy writer drops optional proof reference");
    header.vector_artifact_page = page;
    header.vector_artifact_checksum = checksum;
    db.store
        .update_physical_file_header(header)
        .expect("legacy header");
    db.store.persist().expect("legacy publication");
    db.store
        .publish_detached_native_blob_pages()
        .expect("proof quarantine");
    assert!(db
        .store
        .read_native_artifact_sources(page)
        .expect("legacy sources")
        .is_empty());
    assert!(db
        .prepare_native_artifact_publication(&existing)
        .records
        .iter()
        .any(|(summary, _)| summary.collection == "cold_rows"));
    runtime.checkpoint().expect("regenerate legacy proof");
    assert_artifact_matches_source(&db);
}
