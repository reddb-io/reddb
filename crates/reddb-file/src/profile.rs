//! File-level deployment profile vocabulary.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileProfile {
    Embedded,
    Serverless,
    PrimaryReplica,
    Cluster,
}

impl FileProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Serverless => "serverless",
            Self::PrimaryReplica => "primary-replica",
            Self::Cluster => "cluster",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileArtifactKind {
    SingleRdb,
    Manifest,
    Wal,
    Snapshot,
    BootIndex,
    Pack,
    BaseBackup,
    Timeline,
}

impl FileArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleRdb => "single-rdb",
            Self::Manifest => "manifest",
            Self::Wal => "wal",
            Self::Snapshot => "snapshot",
            Self::BootIndex => "boot-index",
            Self::Pack => "pack",
            Self::BaseBackup => "base-backup",
            Self::Timeline => "timeline",
        }
    }
}
