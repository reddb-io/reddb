//! Operational bootstrap planning for deployment topology and runtime config.
//!
//! This module is the CLI/runtime seam for deployment shape. It translates the
//! human container contract (`REDDB_TOPOLOGY`, `REDDB_NODE_ROLE`,
//! `REDDB_CONFIG_FILE`, storage preset/profile/env overrides, and explicit CLI
//! role flags) into the process role, config overlay path, and
//! [`StorageProfileSelection`] that the server runtime already understands.

use crate::storage::{
    DeployProfile, StorageDeployPreset, StoragePackaging, StorageProfileSelection,
};

pub const DEFAULT_CONFIG_FILE_PATH: &str = "/etc/reddb/config.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationalTopology {
    Standalone,
    Serverless,
    PrimaryReplica,
    Cluster,
}

impl OperationalTopology {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "standalone" => Some(Self::Standalone),
            "serverless" => Some(Self::Serverless),
            "primary-replica" => Some(Self::PrimaryReplica),
            "cluster" => Some(Self::Cluster),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Serverless => "serverless",
            Self::PrimaryReplica => "primary-replica",
            Self::Cluster => "cluster",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationalNodeRole {
    Standalone,
    Serverless,
    Primary,
    Replica,
    ClusterMember,
}

impl OperationalNodeRole {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "standalone" => Some(Self::Standalone),
            "serverless" => Some(Self::Serverless),
            "primary" => Some(Self::Primary),
            "replica" => Some(Self::Replica),
            "cluster-member" => Some(Self::ClusterMember),
            _ => None,
        }
    }

    pub const fn process_role(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Replica => "replica",
            Self::Standalone | Self::Serverless | Self::ClusterMember => "standalone",
        }
    }

    pub const fn implied_topology(self) -> OperationalTopology {
        match self {
            Self::Standalone => OperationalTopology::Standalone,
            Self::Serverless => OperationalTopology::Serverless,
            Self::Primary | Self::Replica => OperationalTopology::PrimaryReplica,
            Self::ClusterMember => OperationalTopology::Cluster,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Serverless => "serverless",
            Self::Primary => "primary",
            Self::Replica => "replica",
            Self::ClusterMember => "cluster-member",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OperationalBootstrapInput {
    /// Command-level override. `red replica` sets this to `replica`; it wins
    /// over all env and flag input.
    pub forced_role: Option<String>,
    /// `red server --role <standalone|primary|replica>`.
    pub role_flag: Option<String>,
    /// Human deployment topology, normally `REDDB_TOPOLOGY`.
    pub topology: Option<String>,
    /// Human node role, normally `REDDB_NODE_ROLE`.
    pub node_role: Option<String>,
    pub storage_preset: Option<String>,
    pub storage_profile: Option<String>,
    pub storage_packaging: Option<String>,
    pub replica_count: Option<String>,
    pub managed_backup: bool,
    pub wal_retention: bool,
    /// Mounted config file path, normally `REDDB_CONFIG_FILE`.
    pub config_file_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalBootstrapPlan {
    pub topology: OperationalTopology,
    pub node_role: OperationalNodeRole,
    pub process_role: String,
    pub storage_profile: StorageProfileSelection,
    pub config_file_path: String,
}

pub fn resolve_operational_bootstrap(
    input: OperationalBootstrapInput,
) -> Result<OperationalBootstrapPlan, String> {
    let topology = parse_optional_topology(input.topology.as_deref())?;
    let env_node_role = parse_optional_node_role(input.node_role.as_deref())?;
    let process_node_role =
        parse_process_role(input.forced_role.as_deref().or(input.role_flag.as_deref()))?;

    let node_role = if let Some(forced) = input.forced_role.as_deref() {
        parse_process_role(Some(forced))?.expect("forced process role is present")
    } else {
        env_node_role
            .or_else(|| match process_node_role {
                Some(OperationalNodeRole::Primary | OperationalNodeRole::Replica) => {
                    process_node_role
                }
                _ => topology.map(default_node_role_for_topology),
            })
            .or(process_node_role)
            .unwrap_or(OperationalNodeRole::Standalone)
    };

    let topology = topology.unwrap_or_else(|| node_role.implied_topology());
    validate_topology_node_role(topology, node_role)?;

    let process_role = process_node_role
        .unwrap_or(node_role)
        .process_role()
        .to_string();

    let storage_profile = resolve_storage_selection(&input, topology)?;
    let config_file_path = resolve_config_file_path(input.config_file_path.as_deref());

    Ok(OperationalBootstrapPlan {
        topology,
        node_role,
        process_role,
        storage_profile,
        config_file_path,
    })
}

pub fn resolve_config_file_path(raw: Option<&str>) -> String {
    raw.filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_CONFIG_FILE_PATH)
        .to_string()
}

fn parse_optional_topology(raw: Option<&str>) -> Result<Option<OperationalTopology>, String> {
    raw.filter(|value| !value.trim().is_empty())
        .map(|value| {
            OperationalTopology::parse(value).ok_or_else(|| {
                format!(
                    "topology {value:?} is not recognised (expected standalone, serverless, primary-replica, or cluster)"
                )
            })
        })
        .transpose()
}

fn parse_optional_node_role(raw: Option<&str>) -> Result<Option<OperationalNodeRole>, String> {
    raw.filter(|value| !value.trim().is_empty())
        .map(|value| {
            OperationalNodeRole::parse(value).ok_or_else(|| {
                format!(
                    "node role {value:?} is not recognised (expected standalone, serverless, primary, replica, or cluster-member)"
                )
            })
        })
        .transpose()
}

fn parse_process_role(raw: Option<&str>) -> Result<Option<OperationalNodeRole>, String> {
    raw.filter(|value| !value.trim().is_empty())
        .map(|value| match value {
            "standalone" => Ok(OperationalNodeRole::Standalone),
            "primary" => Ok(OperationalNodeRole::Primary),
            "replica" => Ok(OperationalNodeRole::Replica),
            _ => Err(format!(
                "process role {value:?} is not recognised (expected standalone, primary, or replica)"
            )),
        })
        .transpose()
}

fn default_node_role_for_topology(topology: OperationalTopology) -> OperationalNodeRole {
    match topology {
        OperationalTopology::Standalone => OperationalNodeRole::Standalone,
        OperationalTopology::Serverless => OperationalNodeRole::Serverless,
        OperationalTopology::PrimaryReplica => OperationalNodeRole::Primary,
        OperationalTopology::Cluster => OperationalNodeRole::ClusterMember,
    }
}

fn validate_topology_node_role(
    topology: OperationalTopology,
    node_role: OperationalNodeRole,
) -> Result<(), String> {
    let ok = matches!(
        (topology, node_role),
        (
            OperationalTopology::Standalone,
            OperationalNodeRole::Standalone
        ) | (
            OperationalTopology::Serverless,
            OperationalNodeRole::Serverless
        ) | (
            OperationalTopology::Serverless,
            OperationalNodeRole::Standalone
        ) | (
            OperationalTopology::PrimaryReplica,
            OperationalNodeRole::Primary
        ) | (
            OperationalTopology::PrimaryReplica,
            OperationalNodeRole::Replica
        ) | (
            OperationalTopology::Cluster,
            OperationalNodeRole::ClusterMember
        ) | (
            OperationalTopology::Cluster,
            OperationalNodeRole::Standalone
        )
    );
    if ok {
        Ok(())
    } else {
        Err(format!(
            "node role {:?} is not valid for topology {:?}",
            node_role.as_str(),
            topology.as_str()
        ))
    }
}

fn resolve_storage_selection(
    input: &OperationalBootstrapInput,
    topology: OperationalTopology,
) -> Result<StorageProfileSelection, String> {
    let mut selection = if let Some(raw) = input
        .storage_preset
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let preset = StorageDeployPreset::parse(raw).ok_or_else(|| {
            format!(
                "storage preset {raw:?} is not recognised (expected embedded, serverless, primary-replica-dev, primary-replica-small, primary-replica-production-ha, primary-replica-backup, primary-replica-wal-retention, or cluster)"
            )
        })?;
        preset.selection()
    } else {
        default_storage_selection(topology)
    };

    if let Some(raw) = input
        .storage_profile
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        selection.deploy_profile = DeployProfile::parse(raw).ok_or_else(|| {
            format!(
                "storage profile {raw:?} is not recognised (expected embedded, serverless, primary-replica, or cluster)"
            )
        })?;
    }

    if let Some(raw) = input
        .storage_packaging
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        selection.packaging = StoragePackaging::parse(raw).ok_or_else(|| {
            format!(
                "storage packaging {raw:?} is not recognised (expected single-file or operational-directory)"
            )
        })?;
    }

    if let Some(raw) = input
        .replica_count
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        selection.replica_count = raw
            .parse::<u16>()
            .map_err(|_| format!("replica-count must be a non-negative integer, got {raw:?}"))?;
    }

    if input.managed_backup {
        selection.managed_backup = true;
    }
    if input.wal_retention {
        selection.wal_retention = true;
    }

    selection.validate()
}

fn default_storage_selection(topology: OperationalTopology) -> StorageProfileSelection {
    match topology {
        OperationalTopology::Standalone => StorageProfileSelection::embedded_single_file(),
        OperationalTopology::Serverless => StorageDeployPreset::Serverless.selection(),
        OperationalTopology::PrimaryReplica => StorageDeployPreset::PrimaryReplicaDev.selection(),
        OperationalTopology::Cluster => StorageDeployPreset::Cluster.selection(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serverless_topology_defaults_to_serverless_storage_and_standalone_process() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("serverless".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.topology, OperationalTopology::Serverless);
        assert_eq!(plan.node_role, OperationalNodeRole::Serverless);
        assert_eq!(plan.process_role, "standalone");
        assert_eq!(
            plan.storage_profile.deploy_profile,
            DeployProfile::Serverless
        );
    }

    #[test]
    fn primary_replica_node_role_selects_process_role() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("primary-replica".to_string()),
            node_role: Some("replica".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.node_role, OperationalNodeRole::Replica);
        assert_eq!(plan.process_role, "replica");
        assert_eq!(
            plan.storage_profile.deploy_profile,
            DeployProfile::PrimaryReplica
        );
    }

    #[test]
    fn cluster_member_uses_cluster_storage_but_standalone_process_role() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("cluster".to_string()),
            node_role: Some("cluster-member".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.topology, OperationalTopology::Cluster);
        assert_eq!(plan.node_role, OperationalNodeRole::ClusterMember);
        assert_eq!(plan.process_role, "standalone");
        assert_eq!(plan.storage_profile.deploy_profile, DeployProfile::Cluster);
    }

    #[test]
    fn standalone_process_role_does_not_hide_cluster_topology_default() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("cluster".to_string()),
            role_flag: Some("standalone".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.node_role, OperationalNodeRole::ClusterMember);
        assert_eq!(plan.process_role, "standalone");
        assert_eq!(plan.storage_profile.deploy_profile, DeployProfile::Cluster);
    }

    #[test]
    fn explicit_storage_preset_wins_over_topology_default() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("serverless".to_string()),
            storage_preset: Some("embedded".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.topology, OperationalTopology::Serverless);
        assert_eq!(plan.storage_profile.deploy_profile, DeployProfile::Embedded);
    }

    #[test]
    fn incompatible_topology_and_node_role_is_rejected() {
        let err = resolve_operational_bootstrap(OperationalBootstrapInput {
            topology: Some("serverless".to_string()),
            node_role: Some("replica".to_string()),
            ..Default::default()
        })
        .unwrap_err();

        assert!(err.contains("not valid for topology"), "{err}");
    }

    #[test]
    fn config_file_path_defaults_to_container_path() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput::default()).unwrap();

        assert_eq!(plan.config_file_path, DEFAULT_CONFIG_FILE_PATH);
    }

    #[test]
    fn explicit_config_file_path_wins() {
        let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
            config_file_path: Some("/custom/reddb.json".to_string()),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(plan.config_file_path, "/custom/reddb.json");
    }
}
