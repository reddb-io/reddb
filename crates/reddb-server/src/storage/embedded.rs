//! Server-facing wrapper for the RedDB file artifact layer.
//!
//! The physical `.rdb` format, WAL, checkpoint, locking, and recovery
//! implementation lives in `reddb-file`. This module keeps the historical
//! `crate::storage::EmbeddedRdbArtifact` API and maps file-layer errors onto
//! `RedDBError`.

use std::path::Path;

use crate::api::{RedDBError, RedDBResult};

pub use reddb_file::{
    EmbeddedRdbManifest, EmbeddedRdbOpen, EmbeddedRdbSuperblock, EMBEDDED_RDB_MANIFEST_0_OFFSET,
    EMBEDDED_RDB_MANIFEST_1_OFFSET, EMBEDDED_RDB_MANIFEST_SLOT_SIZE,
    EMBEDDED_RDB_MANIFEST_ZONE_END, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_1_OFFSET, EMBEDDED_RDB_SUPERBLOCK_SIZE,
};

pub struct EmbeddedRdbArtifact;

impl EmbeddedRdbArtifact {
    pub fn create(path: impl AsRef<Path>) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::create(path))
    }

    pub fn create_with_snapshot(
        path: impl AsRef<Path>,
        snapshot: &[u8],
    ) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::create_with_snapshot(
            path, snapshot,
        ))
    }

    pub fn open(path: impl AsRef<Path>) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::open(path))
    }

    pub fn open_strict_manifest(path: impl AsRef<Path>) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::open_strict_manifest(path))
    }

    pub fn read_snapshot(open: &EmbeddedRdbOpen) -> RedDBResult<Option<Vec<u8>>> {
        map_result(reddb_file::EmbeddedRdbArtifact::read_snapshot(open))
    }

    pub fn write_snapshot(path: impl AsRef<Path>, snapshot: &[u8]) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::write_snapshot(
            path, snapshot,
        ))
    }

    pub fn wal_payloads_encoded_len(payloads: &[Vec<u8>]) -> RedDBResult<u64> {
        map_result(reddb_file::EmbeddedRdbArtifact::wal_payloads_encoded_len(
            payloads,
        ))
    }

    pub fn write_snapshot_with_wal_capacity(
        path: impl AsRef<Path>,
        snapshot: &[u8],
        min_wal_bytes: u64,
    ) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(
            reddb_file::EmbeddedRdbArtifact::write_snapshot_with_wal_capacity(
                path,
                snapshot,
                min_wal_bytes,
            ),
        )
    }

    pub fn read_wal_payloads(open: &EmbeddedRdbOpen) -> RedDBResult<Vec<Vec<u8>>> {
        map_result(reddb_file::EmbeddedRdbArtifact::read_wal_payloads(open))
    }

    pub fn append_wal_payloads(
        path: impl AsRef<Path>,
        payloads: &[Vec<u8>],
    ) -> RedDBResult<EmbeddedRdbOpen> {
        map_result(reddb_file::EmbeddedRdbArtifact::append_wal_payloads(
            path, payloads,
        ))
    }
}

fn map_result<T>(result: reddb_file::RdbFileResult<T>) -> RedDBResult<T> {
    result.map_err(map_error)
}

fn map_error(err: reddb_file::RdbFileError) -> RedDBError {
    match err {
        reddb_file::RdbFileError::InvalidOperation(msg) => RedDBError::InvalidOperation(msg),
        reddb_file::RdbFileError::Io(err) => RedDBError::Io(err),
        // The zone message names the zone and points at scrub/salvage
        // (ADR 0074 §2/§4); keep it verbatim rather than flattening it.
        err @ reddb_file::RdbFileError::ZoneUnrecoverable { .. } => {
            RedDBError::InvalidOperation(err.to_string())
        }
    }
}
