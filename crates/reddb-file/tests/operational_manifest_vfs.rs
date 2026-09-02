use std::io::{self, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use reddb_file::{OpenMode, OperationalManifest, StdVfs, Vfs, VfsDirEntry, VfsFile};

#[derive(Clone, Default)]
struct RecordingVfs {
    events: Arc<Mutex<Vec<String>>>,
}

impl RecordingVfs {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn record(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
}

struct RecordingFile {
    file: <StdVfs as Vfs>::File,
    path: String,
    vfs: RecordingVfs,
}

impl VfsFile for RecordingFile {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.vfs.record(format!("write:{}", self.path));
        self.file.write_all(buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.vfs.record(format!("sync_file:{}", self.path));
        self.file.sync_all()
    }

    // Delegated to the real file: this fixture records *ordering* of the
    // calls the manifest makes, not their semantics, so anything it does not
    // record simply passes through.
    fn try_clone(&self) -> io::Result<Self> {
        Ok(RecordingFile {
            file: self.file.try_clone()?,
            path: self.path.clone(),
            vfs: self.vfs.clone(),
        })
    }

    fn file_len(&self) -> io::Result<u64> {
        self.file.file_len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
}

impl Vfs for RecordingVfs {
    type File = RecordingFile;

    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Self::File> {
        self.record(format!("open:{}", path.display()));
        StdVfs.open(path, mode).map(|file| RecordingFile {
            file,
            path: path.display().to_string(),
            vfs: self.clone(),
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.record(format!("rename:{}->{}", from.display(), to.display()));
        StdVfs.rename(from, to)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.record(format!("sync_dir:{}", dir.display()));
        StdVfs.sync_dir(dir)
    }

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.record(format!("create_dir_all:{}", dir.display()));
        StdVfs.create_dir_all(dir)
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Vec<VfsDirEntry>> {
        StdVfs.read_dir(dir)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.record(format!("remove_file:{}", path.display()));
        StdVfs.remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        self.record(format!("remove_dir_all:{}", path.display()));
        StdVfs.remove_dir_all(path)
    }

    fn exists(&self, path: &Path) -> bool {
        StdVfs.exists(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        StdVfs.is_file(path)
    }
}

#[test]
fn collection_create_orders_physical_durability_before_manifest_publication() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("data.rdb");
    let vfs = RecordingVfs::default();
    let manifest = OperationalManifest::with_vfs(&db_path, vfs.clone());

    manifest.create_collection("users").unwrap();

    let events = vfs.events();
    let physical_sync = events
        .iter()
        .position(|event| event.contains("sync_file:") && event.ends_with("users.rcol"))
        .expect("collection file must be made durable");
    let manifest_rename = events
        .iter()
        .position(|event| {
            event.contains("manifest.json.next->") && event.ends_with("manifest.json")
        })
        .expect("manifest must be atomically published");
    assert!(
        physical_sync < manifest_rename,
        "physical file must be durable before manifest publication: {events:?}"
    );
}

#[test]
fn collection_drop_publishes_pending_before_removing_the_physical_file() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("data.rdb");
    let vfs = RecordingVfs::default();
    let manifest = OperationalManifest::with_vfs(&db_path, vfs.clone());
    manifest.create_collection("users").unwrap();
    let events_before_drop = vfs.events().len();

    let drop = manifest.prepare_collection_drop("users").unwrap();
    drop.finish().unwrap();

    let events = &vfs.events()[events_before_drop..];
    let pending_publish = events
        .iter()
        .position(|event| event.contains("manifest.json.next->"))
        .expect("pending-drop manifest must be published");
    let physical_remove = events
        .iter()
        .position(|event| event.contains("remove_file:") && event.ends_with("users.rcol"))
        .expect("collection file must be removed");
    let final_publish = events
        .iter()
        .rposition(|event| event.contains("manifest.json.next->"))
        .expect("final manifest must be published");
    assert!(
        pending_publish < physical_remove && physical_remove < final_publish,
        "drop must publish pending, remove physical, then publish final: {events:?}"
    );
}

#[test]
fn manifest_write_path_has_one_file_owned_ordering_site() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/operational_manifest/mod.rs",
        "src/operational_manifest/fork.rs",
    ] {
        let source = std::fs::read_to_string(root.join(relative)).unwrap();
        let production = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in ["std::fs", "fs::", "File::", "OpenOptions"] {
            assert!(
                !production.contains(forbidden),
                "manifest writes must use Vfs; found {forbidden:?} in {relative}"
            );
        }
    }

    let server_root = root.join("../reddb-server/src");
    for relative in [
        "storage/unified/store/impl_entities.rs",
        "storage/unified/store/impl_pages.rs",
    ] {
        let source = std::fs::read_to_string(server_root.join(relative)).unwrap();
        for forbidden in [
            "publish_operational_collection_create",
            "publish_operational_collection_pending_drop",
            "publish_operational_collection_drop_finished",
        ] {
            assert!(
                !source.contains(forbidden),
                "manifest phase reconstruction returned in {relative}: {forbidden}"
            );
        }
    }
}
