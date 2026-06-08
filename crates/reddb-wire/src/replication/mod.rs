//! Replication protocol payload contracts.
//!
//! These structs describe the transport-agnostic payloads used by
//! primary/replica exchange. Applying WAL, creating base backups,
//! staging rebootstrap, and failover policy stay in `reddb-server`
//! and `reddb-file`.

mod util;

pub mod basebackup;
pub mod bookmark;
pub mod catchup;
pub mod change_record;
pub mod timeline;
pub mod wal_stream;

pub use basebackup::{BaseBackupChunk, BaseBackupManifestChunk, BaseBackupRequest};
pub use bookmark::{BookmarkDecodeError, CausalBookmark};
pub use catchup::{CatchupMode, CatchupModeReply};
pub use change_record::{
    change_record_json_value_to_string, parse_change_record_json_value, public_item_kind,
    ChangeOperation, ChangeRecord, ChangeRecordJsonValue, DEFAULT_REPLICATION_TERM,
};
pub use timeline::{RejoinPlanNotice, TimelineForkNotice};
pub use util::ReplicationPayloadError;
pub use wal_stream::{WalStreamAck, WalStreamChunk, WalStreamOpen, WalStreamRecord};
