use reddb_file::{OpenMode, StdVfs, Vfs, VfsFile};
use std::io::SeekFrom;

#[test]
fn std_vfs_preserves_file_and_directory_operations() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source = directory.path().join("source");
    let target = directory.path().join("target");
    let vfs = StdVfs;

    let mut file = vfs
        .open(&source, OpenMode::create_truncate())
        .expect("source should open for writing");
    file.write_all(b"reddb")
        .expect("source contents should be written");
    file.sync_all().expect("source contents should be durable");
    drop(file);

    vfs.rename(&source, &target)
        .expect("source should be renamed");
    vfs.sync_dir(directory.path())
        .expect("renamed directory entry should be durable");

    let mut file = vfs
        .open(&target, OpenMode::create_keep())
        .expect("target should open without truncation");
    file.seek(SeekFrom::Start(0))
        .expect("target should seek to its start");
    let mut bytes = [0; 5];
    let count = file.read(&mut bytes).expect("target should be read");

    assert_eq!(count, bytes.len());
    assert_eq!(&bytes, b"reddb");
}
