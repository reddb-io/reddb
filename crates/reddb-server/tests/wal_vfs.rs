use reddb_file::{OpenMode, Vfs, VfsFile};
use reddb_server::storage::wal::{WalRecord, WalWriter};
use std::io::{self, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open {
        read: bool,
        write: bool,
        create: bool,
        truncate: bool,
    },
    Write(Vec<u8>),
    Sync,
}

#[derive(Debug, Clone, Default)]
struct RecordingVfs {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    bytes: Vec<u8>,
    events: Vec<Event>,
}

#[derive(Debug)]
struct RecordingFile {
    state: Arc<Mutex<State>>,
    position: u64,
}

impl VfsFile for RecordingFile {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            state: Arc::clone(&self.state),
            position: self.position,
        })
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self
            .state
            .lock()
            .expect("recording VFS lock should not be poisoned")
            .bytes
            .len() as u64)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        let len = usize::try_from(len).expect("test length should fit usize");
        self.state
            .lock()
            .expect("recording VFS lock should not be poisoned")
            .bytes
            .resize(len, 0);
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .expect("recording VFS lock should not be poisoned");
        let position = usize::try_from(self.position).expect("test position should fit usize");
        let end = position + bytes.len();
        let new_len = state.bytes.len().max(end);
        state.bytes.resize(new_len, 0);
        state.bytes[position..end].copy_from_slice(bytes);
        state.events.push(Event::Write(bytes.to_vec()));
        self.position = end as u64;
        Ok(())
    }

    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let state = self
            .state
            .lock()
            .expect("recording VFS lock should not be poisoned");
        let position = usize::try_from(self.position).expect("test position should fit usize");
        let available = state.bytes.len().saturating_sub(position);
        let count = available.min(bytes.len());
        bytes[..count].copy_from_slice(&state.bytes[position..position + count]);
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let len = self
            .state
            .lock()
            .expect("recording VFS lock should not be poisoned")
            .bytes
            .len() as u64;
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::End(delta) => len.saturating_add_signed(delta),
            SeekFrom::Current(delta) => self.position.saturating_add_signed(delta),
        };
        Ok(self.position)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.state
            .lock()
            .expect("recording VFS lock should not be poisoned")
            .events
            .push(Event::Sync);
        Ok(())
    }
}

impl Vfs for RecordingVfs {
    type File = RecordingFile;

    fn open(&self, _path: &Path, mode: OpenMode) -> io::Result<Self::File> {
        let mut state = self
            .state
            .lock()
            .expect("recording VFS lock should not be poisoned");
        if mode.truncate {
            state.bytes.clear();
        }
        state.events.push(Event::Open {
            read: mode.read,
            write: mode.write,
            create: mode.create,
            truncate: mode.truncate,
        });
        drop(state);
        Ok(RecordingFile {
            state: Arc::clone(&self.state),
            position: 0,
        })
    }

    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        unreachable!("WAL append does not rename files")
    }

    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        unreachable!("WAL append does not sync directories")
    }
}

#[test]
fn wal_append_and_sync_use_the_injected_vfs_in_order() {
    let vfs = RecordingVfs::default();
    let mut writer = WalWriter::open_with_vfs(vfs.clone(), Path::new("wal"))
        .expect("WAL should open through the injected VFS");
    let record = WalRecord::Begin { tx_id: 42 };

    writer
        .append(&record)
        .expect("record should append through the VFS");
    writer.sync().expect("record should sync through the VFS");

    let events = vfs
        .state
        .lock()
        .expect("recording VFS lock should not be poisoned")
        .events
        .clone();
    assert_eq!(
        events,
        vec![
            Event::Open {
                read: true,
                write: true,
                create: true,
                truncate: false,
            },
            Event::Write(reddb_file::encode_wal_file_header().to_vec()),
            Event::Sync,
            Event::Write(record.encode()),
            Event::Sync,
        ]
    );
}
