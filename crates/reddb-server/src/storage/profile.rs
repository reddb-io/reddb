//! Storage/deploy profile selection contract.
//!
//! This module is intentionally pure: it declares the operator-facing
//! profile/package/preset vocabulary and validates combinations before later
//! storage layout work chooses concrete directories.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployProfile {
    Embedded,
    Serverless,
    PrimaryReplica,
    Cluster,
}

impl DeployProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Serverless => "serverless",
            Self::PrimaryReplica => "primary-replica",
            Self::Cluster => "cluster",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match normalize(raw).as_str() {
            "embedded" => Some(Self::Embedded),
            "serverless" => Some(Self::Serverless),
            "primaryreplica" | "primary" | "replica" => Some(Self::PrimaryReplica),
            "cluster" => Some(Self::Cluster),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoragePackaging {
    SingleFile,
    OperationalDirectory,
}

impl StoragePackaging {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleFile => "single-file",
            Self::OperationalDirectory => "operational-directory",
        }
    }

    pub const fn is_operational(self) -> bool {
        matches!(self, Self::OperationalDirectory)
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match normalize(raw).as_str() {
            "singlefile" | "embedded" => Some(Self::SingleFile),
            "operationaldirectory" | "operational" | "directory" | "dir" => {
                Some(Self::OperationalDirectory)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageDeployPreset {
    Embedded,
    Serverless,
    PrimaryReplicaDev,
    PrimaryReplicaSmall,
    PrimaryReplicaProductionHa,
    PrimaryReplicaBackup,
    PrimaryReplicaWalRetention,
    Cluster,
}

impl StorageDeployPreset {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Serverless => "serverless",
            Self::PrimaryReplicaDev => "primary-replica-dev",
            Self::PrimaryReplicaSmall => "primary-replica-small",
            Self::PrimaryReplicaProductionHa => "primary-replica-production-ha",
            Self::PrimaryReplicaBackup => "primary-replica-backup",
            Self::PrimaryReplicaWalRetention => "primary-replica-wal-retention",
            Self::Cluster => "cluster",
        }
    }

    pub const fn selection(self) -> StorageProfileSelection {
        match self {
            Self::Embedded => StorageProfileSelection {
                deploy_profile: DeployProfile::Embedded,
                packaging: StoragePackaging::SingleFile,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            },
            Self::Serverless => StorageProfileSelection {
                deploy_profile: DeployProfile::Serverless,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 0,
                managed_backup: true,
                wal_retention: true,
            },
            Self::PrimaryReplicaDev => StorageProfileSelection {
                deploy_profile: DeployProfile::PrimaryReplica,
                packaging: StoragePackaging::SingleFile,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            },
            Self::PrimaryReplicaSmall => StorageProfileSelection {
                deploy_profile: DeployProfile::PrimaryReplica,
                packaging: StoragePackaging::SingleFile,
                replica_count: 1,
                managed_backup: false,
                wal_retention: false,
            },
            Self::PrimaryReplicaProductionHa => StorageProfileSelection {
                deploy_profile: DeployProfile::PrimaryReplica,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 2,
                managed_backup: false,
                wal_retention: false,
            },
            Self::PrimaryReplicaBackup => StorageProfileSelection {
                deploy_profile: DeployProfile::PrimaryReplica,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 1,
                managed_backup: true,
                wal_retention: false,
            },
            Self::PrimaryReplicaWalRetention => StorageProfileSelection {
                deploy_profile: DeployProfile::PrimaryReplica,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 1,
                managed_backup: false,
                wal_retention: true,
            },
            Self::Cluster => StorageProfileSelection {
                deploy_profile: DeployProfile::Cluster,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 3,
                managed_backup: true,
                wal_retention: true,
            },
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match normalize(raw).as_str() {
            "embedded" => Some(Self::Embedded),
            "serverless" => Some(Self::Serverless),
            "primaryreplicadev" | "replicadev" | "dev" => Some(Self::PrimaryReplicaDev),
            "primaryreplicasmall" | "replicasmall" | "small" => Some(Self::PrimaryReplicaSmall),
            "primaryreplicaproductionha" | "productionha" | "ha" => {
                Some(Self::PrimaryReplicaProductionHa)
            }
            "primaryreplicabackup" | "backup" => Some(Self::PrimaryReplicaBackup),
            "primaryreplicawalretention" | "walretention" | "pitr" => {
                Some(Self::PrimaryReplicaWalRetention)
            }
            "cluster" => Some(Self::Cluster),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageProfileSelection {
    pub deploy_profile: DeployProfile,
    pub packaging: StoragePackaging,
    pub replica_count: u16,
    pub managed_backup: bool,
    pub wal_retention: bool,
}

impl StorageProfileSelection {
    pub const fn embedded_single_file() -> Self {
        StorageDeployPreset::Embedded.selection()
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.deploy_profile == DeployProfile::Cluster && !self.packaging.is_operational() {
            return Err(
                "storage deploy profile `cluster` requires storage packaging `operational-directory`; embedded single-file packaging is not allowed"
                    .to_string(),
            );
        }

        if self.deploy_profile == DeployProfile::PrimaryReplica
            && !self.packaging.is_operational()
            && (self.managed_backup || self.wal_retention || self.replica_count > 1)
        {
            let reason = if self.managed_backup {
                "managed backup"
            } else if self.wal_retention {
                "WAL retention"
            } else {
                "more than one replica"
            };
            return Err(format!(
                "production primary-replica deployment with {reason} requires storage packaging `operational-directory`"
            ));
        }

        if matches!(
            self.deploy_profile,
            DeployProfile::Serverless | DeployProfile::Cluster
        ) && self.replica_count > 0
            && self.deploy_profile != DeployProfile::Cluster
        {
            return Err(format!(
                "storage deploy profile `{}` does not accept replica_count={}",
                self.deploy_profile.as_str(),
                self.replica_count
            ));
        }

        Ok(self)
    }
}

fn normalize(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_primary_replica_allows_single_file() {
        StorageDeployPreset::PrimaryReplicaDev
            .selection()
            .validate()
            .expect("dev primary-replica preset should allow single-file packaging");
        StorageDeployPreset::PrimaryReplicaSmall
            .selection()
            .validate()
            .expect("small primary-replica preset should allow single-file packaging");
    }

    #[test]
    fn production_primary_replica_requires_operational_directory() {
        let err = StorageProfileSelection {
            deploy_profile: DeployProfile::PrimaryReplica,
            packaging: StoragePackaging::SingleFile,
            replica_count: 2,
            managed_backup: false,
            wal_retention: false,
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("production primary-replica"));
        assert!(err.contains("operational-directory"));

        let err = StorageProfileSelection {
            deploy_profile: DeployProfile::PrimaryReplica,
            packaging: StoragePackaging::SingleFile,
            replica_count: 1,
            managed_backup: true,
            wal_retention: false,
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("managed backup"));

        let err = StorageProfileSelection {
            deploy_profile: DeployProfile::PrimaryReplica,
            packaging: StoragePackaging::SingleFile,
            replica_count: 1,
            managed_backup: false,
            wal_retention: true,
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("WAL retention"));
    }

    #[test]
    fn production_presets_select_operational_directory() {
        for preset in [
            StorageDeployPreset::PrimaryReplicaProductionHa,
            StorageDeployPreset::PrimaryReplicaBackup,
            StorageDeployPreset::PrimaryReplicaWalRetention,
            StorageDeployPreset::Cluster,
        ] {
            let selection = preset.selection().validate().expect(preset.as_str());
            assert_eq!(selection.packaging, StoragePackaging::OperationalDirectory);
        }
    }

    #[test]
    fn cluster_rejects_single_file_packaging() {
        let err = StorageProfileSelection {
            deploy_profile: DeployProfile::Cluster,
            packaging: StoragePackaging::SingleFile,
            replica_count: 3,
            managed_backup: false,
            wal_retention: false,
        }
        .validate()
        .unwrap_err();
        assert!(err.contains("cluster"));
        assert!(err.contains("embedded single-file"));
    }
}
