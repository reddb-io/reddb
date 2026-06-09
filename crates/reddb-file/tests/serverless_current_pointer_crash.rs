use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use reddb_file::{
    ServerlessExtentIndex, ServerlessExtentRef, ServerlessFilePlan, ServerlessManifest,
};

const CHILD_ENV: &str = "REDDB_SERVERLESS_POINTER_CRASH_CHILD";
const ROOT_ENV: &str = "REDDB_SERVERLESS_POINTER_CRASH_ROOT";
const CRASH_ENV: &str = "REDDB_SERVERLESS_CRASH_AT";

#[test]
fn current_pointer_publish_survives_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "current_pointer_after_tmp_write",
        "current_pointer_after_tmp_sync",
        "current_pointer_after_rename",
        "current_pointer_after_dir_sync",
    ] {
        let root = temp_root(point);
        let first = ServerlessFilePlan::new(root.path(), "db", 1);
        let first_pointer = first
            .publish_core_generation(&extent_index(1, b"first"), b"first", b"secondary")
            .expect("publish first generation");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("serverless_current_pointer_crash_child")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, root.path())
            .env(CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let current = first
            .read_current_pointer()
            .expect("current pointer decodes");
        assert!(
            current.generation == 1 || current.generation == 2,
            "current pointer must be old or new, got generation {} after {point}",
            current.generation
        );
        if current.generation == 1 {
            assert_eq!(current, first_pointer);
        } else {
            let second = ServerlessFilePlan::new(root.path(), "db", 2);
            let manifest =
                ServerlessManifest::read_from_path(second.manifest_path()).expect("read manifest");
            second
                .validate_complete_generation(&manifest)
                .expect("new generation is complete when pointer advanced");
        }
    }
}

#[test]
fn generation_pack_publish_does_not_advance_current_on_crash() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "serverless_pack_after_tmp_write",
        "serverless_pack_after_tmp_sync",
        "serverless_pack_after_rename",
        "serverless_pack_after_dir_sync",
    ] {
        let root = temp_root(point);
        let first = ServerlessFilePlan::new(root.path(), "db", 1);
        let first_pointer = first
            .publish_core_generation(&extent_index(1, b"first"), b"first", b"secondary")
            .expect("publish first generation");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("serverless_current_pointer_crash_child")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, root.path())
            .env(CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let current = first
            .read_current_pointer_verified()
            .expect("current pointer remains verified");
        assert_eq!(
            current, first_pointer,
            "pack crash at {point} must not advance CURRENT"
        );
    }
}

#[test]
fn serverless_current_pointer_crash_child() -> ExitCode {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let second = ServerlessFilePlan::new(&root, "db", 2);
    let _ = second.publish_core_generation(&extent_index(2, b"second"), b"second", b"secondary");
    ExitCode::from(1)
}

fn extent_index(generation: u64, payload: &[u8]) -> ServerlessExtentIndex {
    let mut index = ServerlessExtentIndex::new(generation);
    index.push(
        ServerlessExtentRef::new(
            "events",
            b"a".to_vec(),
            b"z".to_vec(),
            Path::new("collection-data.redpack"),
            0,
            payload,
            true,
        )
        .expect("extent"),
    );
    index
}

/// Auto-cleaning temp root: the returned [`tempfile::TempDir`] guard removes the
/// directory and all artifacts under it on drop, including on panic. The caller
/// keeps the binding alive across the crash child + assertions and reads the
/// path via `root.path()`.
fn temp_root(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-serverless-current-crash-{label}-"))
        .tempdir()
        .expect("temp dir")
}
