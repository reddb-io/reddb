use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use reddb_server::storage::backend::{BackendError, LocalBackend, RemoteBackend};
use reddb_server::storage::wal::PointInTimeRecovery;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::{
    BiometricFormat, BiometricSample, BitSignature, FeatureVector, LandmarkSet, Value,
};

const BIOMETRIC_SPACE: [u8; 16] = [0x61; 16];

fn fingerprint_sample() -> BiometricSample {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FMR\0 20\0");
    bytes.extend_from_slice(&36_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&[1, 0, 100, 1]);
    bytes.extend_from_slice(&(100_u16 | 0x4000).to_be_bytes());
    bytes.extend_from_slice(&120_u16.to_be_bytes());
    bytes.extend_from_slice(&[10, 90, 0, 0]);
    BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, bytes)
        .expect("standard fingerprint minutiae")
}

fn typed_values() -> [Value; 6] {
    let asn1 = vec![0x30, 0x03, 0x02, 0x01, 0x01];
    [
        Value::Fingerprint(fingerprint_sample()),
        Value::Face(
            BiometricSample::with_format(BiometricFormat::FaceIso39794_5, asn1.clone())
                .expect("standard face record"),
        ),
        Value::Iris(
            BiometricSample::with_format(BiometricFormat::IrisIso39794_6, asn1)
                .expect("standard iris record"),
        ),
        Value::FeatureVector(
            FeatureVector::new(BIOMETRIC_SPACE, vec![1.0, -0.5]).expect("feature vector"),
        ),
        Value::BitSignature(
            BitSignature::new(BIOMETRIC_SPACE, vec![0xaa, 0x55], Some(vec![0xff, 0x0f]))
                .expect("bit signature"),
        ),
        Value::LandmarkSet(
            LandmarkSet::new(
                BIOMETRIC_SPACE,
                2,
                vec![
                    ([-1.0, 1.0, 0.0], [1.0, 1.0, 0.0]),
                    ([-2.0, -1.0, 0.0], [2.0, -1.0, 0.0]),
                ],
                vec![[0.0, 2.0, 0.0]],
            )
            .expect("landmark set"),
        ),
    ]
}

struct SplitSnapshotBackend {
    started: Mutex<Option<mpsc::Sender<()>>>,
    resume: Mutex<mpsc::Receiver<()>>,
    split_once: AtomicBool,
}

impl RemoteBackend for SplitSnapshotBackend {
    fn name(&self) -> &str {
        "split-snapshot"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        LocalBackend.download(remote_key, local_path)
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        if reddb_file::is_archived_snapshot_key(remote_key)
            && !self.split_once.swap(true, Ordering::AcqRel)
        {
            let mut source = std::fs::File::open(local_path)
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            let length = source
                .metadata()
                .map_err(|error| BackendError::Transport(error.to_string()))?
                .len();
            let destination = Path::new(remote_key);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
            }
            let mut output = std::fs::File::create(destination)
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            std::io::copy(&mut Read::by_ref(&mut source).take(length / 2), &mut output)
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            if let Some(started) = self.started.lock().expect("started lock").take() {
                started
                    .send(())
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
            }
            self.resume
                .lock()
                .expect("resume lock")
                .recv()
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            std::io::copy(&mut source, &mut output)
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            output
                .sync_all()
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            return Ok(());
        }
        LocalBackend.upload(local_path, remote_key)
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        LocalBackend.exists(remote_key)
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        LocalBackend.delete(remote_key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        LocalBackend.list(prefix)
    }
}

#[test]
fn populated_single_file_backup_restores_into_an_independent_store() {
    let directory = tempfile::tempdir().expect("private synthetic test directory");
    let source = directory.path().join("source").join("data.rdb");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let wal = format!("{}/", remote.join("wal").display());
    let backend = Arc::new(LocalBackend);
    let options = RedDBOptions::persistent(&source)
        .with_remote_backend(backend.clone(), remote.join("head.rdb").to_string_lossy())
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots)
        .with_metadata("red.config.wal.archive.prefix", &wal)
        .with_metadata(
            "red.config.backup.head_key",
            remote.join("HEAD.json").to_string_lossy(),
        );
    let runtime = RedDBRuntime::with_options(options).expect("source runtime");
    for id in 0..25 {
        runtime
            .execute_query(&format!(
                "INSERT INTO backup_rows (id, name) VALUES ({id}, 'row-{id}')"
            ))
            .expect("synthetic row");
    }
    runtime
        .execute_query(
            "CREATE TABLE backup_biometrics (id INTEGER, print FINGERPRINT, face FACE, iris IRIS, features FEATURE_VECTOR, signature BIT_SIGNATURE, landmarks LANDMARK_SET)",
        )
        .expect("typed schema");
    let values = typed_values();
    let mut parameters = vec![Value::Integer(1)];
    parameters.extend(values.iter().cloned());
    runtime
        .execute_query_with_params(
            "INSERT INTO backup_biometrics (id,print,face,iris,features,signature,landmarks) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            &parameters,
        )
        .expect("typed backup row");
    let expected = runtime
        .execute_query("SELECT id, name FROM backup_rows ORDER BY id")
        .expect("source readback")
        .result;
    assert_eq!(expected.len(), 25);
    let backup = runtime.trigger_backup().expect("runtime backup");
    assert!(backup.uploaded, "backup must reach the configured backend");
    let destination = directory.path().join("independent").join("data.rdb");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let recovery = PointInTimeRecovery::new(backend, &snapshots, &wal);
    recovery
        .restore_to(0, &destination)
        .expect("independent restore");
    let restored_options = RedDBOptions::persistent(&destination);
    let restored = RedDBRuntime::with_options(restored_options).expect("restored runtime");
    let actual = restored
        .execute_query("SELECT id, name FROM backup_rows ORDER BY id")
        .expect("restored readback")
        .result;
    let rows = |result: &reddb_server::storage::query::unified::UnifiedResult| {
        result
            .records
            .iter()
            .map(|record| (record.get("id").cloned(), record.get("name").cloned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(&actual),
        rows(&expected),
        "all populated single-file rows must survive backup/restore"
    );
    let restored_typed = restored
        .execute_query(
            "SELECT print,face,iris,features,signature,landmarks FROM backup_biometrics WHERE id = 1",
        )
        .expect("restored typed readback")
        .result;
    assert_eq!(restored_typed.records.len(), 1);
    for (column, expected) in [
        "print",
        "face",
        "iris",
        "features",
        "signature",
        "landmarks",
    ]
    .into_iter()
    .zip(values.iter())
    {
        assert_eq!(
            restored_typed.records[0].get(column),
            Some(expected),
            "{column} must survive operational backup/restore"
        );
    }
}

#[test]
fn concurrent_update_ddl_and_vacuum_leave_a_restorable_backup_cut() {
    let directory = tempfile::tempdir().expect("private concurrent backup directory");
    let source = directory.path().join("source").join("data.rdb");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let wal = format!("{}/", remote.join("wal").display());
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let backend: Arc<dyn RemoteBackend> = Arc::new(SplitSnapshotBackend {
        started: Mutex::new(Some(started_tx)),
        resume: Mutex::new(resume_rx),
        split_once: AtomicBool::new(false),
    });
    let options = RedDBOptions::persistent(&source)
        .with_remote_backend(backend.clone(), remote.join("head.rdb").to_string_lossy())
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots)
        .with_metadata("red.config.wal.archive.prefix", &wal)
        .with_metadata(
            "red.config.backup.head_key",
            remote.join("HEAD.json").to_string_lossy(),
        );
    let runtime = RedDBRuntime::with_options(options).expect("source runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("deterministic vacuum");
    runtime
        .execute_query("CREATE TABLE backup_cut (id INT, generation INT, payload TEXT)")
        .expect("table");
    runtime
        .execute_query("CREATE UNIQUE INDEX backup_cut_id ON backup_cut (id) USING HASH")
        .expect("hash index");
    runtime
        .execute_query("CREATE INDEX backup_cut_generation ON backup_cut (generation) USING BTREE")
        .expect("btree index");
    for id in 0..128 {
        runtime
            .execute_query(&format!(
                "INSERT INTO backup_cut (id,generation,payload) VALUES ({id},1,'baseline-{id}')"
            ))
            .expect("baseline row");
    }

    std::thread::scope(|scope| {
        let backup = scope.spawn(|| runtime.trigger_backup());
        started_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("snapshot upload reached its midpoint");
        let update = runtime.execute_query("UPDATE backup_cut SET generation = 2");
        let ddl = runtime.execute_query("ALTER TABLE backup_cut ADD COLUMN during_backup TEXT");
        let vacuum = runtime.execute_query("VACUUM backup_cut");
        let checkpoint = runtime.checkpoint();
        resume_tx.send(()).expect("finish snapshot upload");
        update.expect("concurrent update");
        ddl.expect("concurrent DDL");
        vacuum.expect("concurrent vacuum");
        checkpoint.expect("publish concurrent generation");
        let result = backup
            .join()
            .expect("backup thread")
            .expect("backup result");
        assert!(result.uploaded);
    });

    let destination = directory.path().join("restore").join("data.rdb");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    PointInTimeRecovery::new(backend, &snapshots, &wal)
        .restore_to(0, &destination)
        .expect("concurrent backup must remain restorable");
    let restored = RedDBRuntime::with_options(RedDBOptions::persistent(&destination))
        .expect("restored runtime");
    let rows = restored
        .execute_query("SELECT id, generation FROM backup_cut ORDER BY id")
        .expect("restored rows");
    assert_eq!(rows.result.records.len(), 128);
    assert!(rows
        .result
        .records
        .iter()
        .all(|row| { row.get("generation") == Some(&reddb_types::Value::Integer(1)) }));
    let hash = restored
        .execute_query("SELECT id FROM backup_cut WHERE id = 64")
        .expect("cold restored HASH lookup");
    assert_eq!(hash.result.records.len(), 1);
    let btree = restored
        .execute_query("SELECT id FROM backup_cut WHERE generation = 1 ORDER BY id")
        .expect("cold restored BTREE lookup");
    assert_eq!(btree.result.records.len(), 128);
    let described = restored
        .execute_query("DESCRIBE backup_cut")
        .expect("restored schema");
    assert!(described
        .result
        .records
        .iter()
        .all(|row| row.get("name") != Some(&reddb_types::Value::text("during_backup"))));
}

#[test]
fn operational_backup_refuses_an_unrestorable_main_file_snapshot() {
    let directory = tempfile::tempdir().expect("private synthetic test directory");
    let source = directory.path().join("source").join("data.rdb");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let mut options = RedDBOptions::persistent(&source)
        .with_remote_backend(
            Arc::new(LocalBackend),
            remote.join("head.rdb").to_string_lossy(),
        )
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots);
    options.storage_profile.packaging =
        reddb_server::storage::StoragePackaging::OperationalDirectory;
    let runtime = RedDBRuntime::with_options(options).expect("operational source runtime");
    runtime
        .execute_query("INSERT INTO backup_rows (id, name) VALUES (1, 'preserved')")
        .unwrap();
    assert!(!source
        .with_extension("rdb.ops")
        .join("collections")
        .is_dir());
    let error = runtime
        .trigger_backup()
        .expect_err("incomplete backup must fail");
    assert!(error
        .to_string()
        .contains("checkpoint-consistent physical bundle"));
    assert!(
        !std::path::Path::new(&snapshots).exists(),
        "must not advertise a snapshot"
    );
    let status = runtime.backup_status();
    assert_eq!(status.total_backups, 0);
    assert!(status.last_backup.is_none());
    assert_eq!(
        runtime
            .execute_query("SELECT id, name FROM backup_rows")
            .unwrap()
            .result
            .len(),
        1
    );
}

#[test]
fn embedded_options_do_not_allow_incomplete_backup_of_an_existing_paged_store() {
    let directory = tempfile::tempdir().expect("private synthetic store");
    let source = directory.path().join("legacy.rdb");
    {
        let store = reddb_server::storage::UnifiedStore::open(&source).expect("paged store");
        store.create_collection("seed").expect("seed collection");
        store.persist().expect("paged publication");
    }
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let options = RedDBOptions::persistent(&source)
        .with_remote_backend(
            Arc::new(LocalBackend),
            remote.join("head.rdb").to_string_lossy(),
        )
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots);
    let runtime = RedDBRuntime::with_options(options).expect("legacy runtime");
    assert!(runtime.db().is_paged(), "actual file format is paged");
    runtime
        .execute_query("CREATE DOCUMENT backup_contract_probe")
        .expect("durable document definition");
    runtime
        .execute_query(
            "INSERT INTO backup_contract_probe DOCUMENT VALUES ({\"name\":\"preserved\"})",
        )
        .expect("source document");

    let error = runtime
        .trigger_backup()
        .expect_err("a main-file-only backup would lose the collection definition");
    assert!(error
        .to_string()
        .contains("checkpoint-consistent physical bundle"));
    assert!(
        !std::path::Path::new(&snapshots).exists(),
        "no incomplete snapshot advertised"
    );
    assert_eq!(runtime.backup_status().total_backups, 0);
    let collection = runtime
        .catalog()
        .collections
        .into_iter()
        .find(|collection| collection.name == "backup_contract_probe")
        .expect("source collection remains available");
    assert_eq!(
        collection.model,
        reddb_server::catalog::CollectionModel::Document
    );
    assert_eq!(
        runtime
            .execute_query("SELECT name FROM backup_contract_probe")
            .expect("source readback")
            .result
            .len(),
        1
    );
}
