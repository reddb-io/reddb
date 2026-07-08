//! Store-fork lifecycle workload, driven under the `unreliable-libc` shim.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use reddb_file::OperationalManifest;

const DB_FILE: &str = "store.rdb";
const FORK_NAME: &str = "exp";

fn main() -> ExitCode {
    let stage = match std::env::args().nth(1).as_deref() {
        Some("create") => Stage::Create,
        Some("hydrate") => Stage::Hydrate,
        Some("promote") => Stage::Promote,
        Some(other) => {
            eprintln!("unknown store-fork stage: {other}");
            return ExitCode::from(2);
        }
        None => {
            eprintln!("store-fork stage is required");
            return ExitCode::from(2);
        }
    };
    let dir = match std::env::var_os("UNRELIABLE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("UNRELIABLE_DIR is required");
            return ExitCode::from(2);
        }
    };

    match run_stage(&dir, stage) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("store-fork workload stopped on durability error: {err}");
            ExitCode::from(1)
        }
    }
}

#[derive(Clone, Copy)]
enum Stage {
    Create,
    Hydrate,
    Promote,
}

fn run_stage(dir: &Path, stage: Stage) -> std::io::Result<()> {
    let parent = OperationalManifest::for_db_path(&dir.join(DB_FILE));
    match stage {
        Stage::Create => parent.create_fork(FORK_NAME, 42),
        Stage::Hydrate => parent.fork_handle(FORK_NAME).hydrate_collection("users"),
        Stage::Promote => parent.promote_fork(FORK_NAME).map(|_| ()),
    }
}
