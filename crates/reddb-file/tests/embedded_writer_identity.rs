use reddb_file::{EmbeddedRdbArtifact, EmbeddedRdbWriterLock};
use std::path::Path;
use std::process::Command;

fn child(path: &Path, mode: &str) -> std::process::Output {
    Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "identity_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("REDDB_IDENTITY_CHILD_PATH", path)
        .env("REDDB_IDENTITY_CHILD_MODE", mode)
        .output()
        .expect("run child")
}

#[test]
fn identity_child() {
    let Some(path) = std::env::var_os("REDDB_IDENTITY_CHILD_PATH") else {
        return;
    };
    if std::env::var("REDDB_IDENTITY_CHILD_MODE").expect("mode") == "reject" {
        assert!(
            EmbeddedRdbWriterLock::acquire(&path).is_err(),
            "alias writer was admitted"
        );
    } else {
        let _writer = EmbeddedRdbWriterLock::acquire(&path).expect("child writer");
        EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"committed".to_vec()]).expect("commit");
        // Skip destructors: the kernel must release the data-file fence.
        std::process::exit(91);
    }
}

#[test]
fn hardlink_writer_is_refused_in_process_and_across_processes() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("original.rdb");
    let alias_dir = dir.path().join("elsewhere");
    std::fs::create_dir(&alias_dir).expect("alias directory");
    let alias = alias_dir.join("alias.rdb");
    let writer = EmbeddedRdbWriterLock::acquire(&path).expect("create and fence");
    std::fs::hard_link(&path, &alias).expect("hardlink made after writer opened");
    for _ in 0..3 {
        // Closing temporary handles must not release the lifetime fence.
        EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"wal".to_vec()]).expect("append");
        let opened = EmbeddedRdbArtifact::open(&alias).expect("read through alias");
        assert!(!EmbeddedRdbArtifact::read_wal_payloads(&opened)
            .expect("WAL")
            .is_empty());
        let error = EmbeddedRdbWriterLock::acquire(&alias)
            .err()
            .expect("alias refused");
        assert!(
            error.to_string().contains("already open for writing"),
            "{error}"
        );
    }
    let result = child(&alias, "reject");
    assert!(result.status.success(), "{result:?}");
    EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-durable snapshot")
        .expect("checkpoint while fenced");
    drop(writer);
    let second = EmbeddedRdbWriterLock::acquire(&alias).expect("acquire after release");
    let (opened, snapshot) = EmbeddedRdbArtifact::open_with_snapshot(&alias).expect("snapshot");
    assert_eq!(
        snapshot.as_deref(),
        Some(b"RDST-durable snapshot".as_slice())
    );
    assert!(EmbeddedRdbArtifact::read_wal_payloads(&opened)
        .expect("folded WAL")
        .is_empty());
    drop(second);
}

#[test]
fn process_death_releases_identity_fence_without_losing_wal() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("crash.rdb");
    let result = child(&path, "crash");
    assert_eq!(result.status.code(), Some(91), "{result:?}");
    let _writer = EmbeddedRdbWriterLock::acquire(&path).expect("reopen after abrupt exit");
    let opened = EmbeddedRdbArtifact::open(&path).expect("open");
    assert_eq!(
        EmbeddedRdbArtifact::read_wal_payloads(&opened).expect("WAL"),
        vec![b"committed".to_vec()]
    );
}

#[test]
fn rename_does_not_change_the_writer_identity() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("original.rdb");
    let alias = dir.path().join("renamed.rdb");
    let writer = EmbeddedRdbWriterLock::acquire(&path).expect("writer");
    std::fs::rename(&path, &alias).expect("rename");
    assert!(EmbeddedRdbWriterLock::acquire(&alias).is_err());
    std::fs::rename(&alias, &path).expect("restore path before writer operations");
    drop(writer);
    EmbeddedRdbWriterLock::acquire(&path).expect("released");
}

#[cfg(unix)]
#[test]
fn symlink_cannot_bypass_writer_identity() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("original.rdb");
    let alias = dir.path().join("symlink.rdb");
    let _writer = EmbeddedRdbWriterLock::acquire(&path).expect("writer");
    std::os::unix::fs::symlink(&path, &alias).expect("symlink");
    assert!(EmbeddedRdbWriterLock::acquire(&alias).is_err());
}
