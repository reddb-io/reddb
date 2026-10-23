//! Artifact interop fixture driver used by packages/browser's real browser tests.
//! The snapshot and WAL payloads are opaque test bytes, not engine data.

use std::error::Error;
use std::path::PathBuf;

use reddb_file::EmbeddedRdbArtifact;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let action = arguments.next().ok_or("expected create or verify")?;
    let path = PathBuf::from(arguments.next().ok_or("expected artifact path")?);
    if arguments.next().is_some() {
        return Err("unexpected extra arguments".into());
    }
    match action.to_str() {
        Some("create") => {
            EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDSTinterop")?;
            EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"native".to_vec()])?;
        }
        Some("verify") => {
            let open = EmbeddedRdbArtifact::open(&path)?;
            if EmbeddedRdbArtifact::read_snapshot(&open)? != Some(b"RDSTinterop".to_vec()) {
                return Err("browser artifact changed the native snapshot".into());
            }
            if EmbeddedRdbArtifact::read_wal_payloads(&open)?
                != vec![b"native".to_vec(), b"browser".to_vec()]
            {
                return Err("browser artifact did not preserve the committed WAL".into());
            }
        }
        _ => return Err("expected create or verify".into()),
    }
    Ok(())
}
