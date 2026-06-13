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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_profiles_render_canonical_labels() {
        let profiles = [
            (FileProfile::Embedded, "embedded"),
            (FileProfile::Serverless, "serverless"),
            (FileProfile::PrimaryReplica, "primary-replica"),
            (FileProfile::Cluster, "cluster"),
        ];

        for (profile, label) in profiles {
            assert_eq!(profile.as_str(), label);
        }
    }

    #[test]
    fn artifact_kinds_render_canonical_labels() {
        let kinds = [
            (FileArtifactKind::SingleRdb, "single-rdb"),
            (FileArtifactKind::Manifest, "manifest"),
            (FileArtifactKind::Wal, "wal"),
            (FileArtifactKind::Snapshot, "snapshot"),
            (FileArtifactKind::BootIndex, "boot-index"),
            (FileArtifactKind::Pack, "pack"),
            (FileArtifactKind::BaseBackup, "base-backup"),
            (FileArtifactKind::Timeline, "timeline"),
        ];

        for (kind, label) in kinds {
            assert_eq!(kind.as_str(), label);
        }
    }
}
