//! Test-only native host for B-tree/OPFS interoperability.

#[path = "../tests/support/codec_corpus.rs"]
mod codec_corpus;
use reddb_file::{OpenMode, StdVfs, Vfs};
use std::path::PathBuf;

#[path = "../tests/support/btree_restart.rs"]
mod fixture;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let phase = match arguments.next().as_deref() {
        Some("create") => 0,
        Some("verify") => 2,
        _ => return Err("expected create or verify".into()),
    };
    let path = PathBuf::from(arguments.next().ok_or("expected fixture path")?);
    let file = StdVfs.open(
        &path,
        OpenMode {
            create: phase == 0,
            ..OpenMode::create_keep()
        },
    )?;
    fixture::run(file, phase)
}
