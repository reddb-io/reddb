use std::path::Path;

use serde::Deserialize;

use reddb_server::storage::{
    LayoutOverrides, LayoutToggles, LogDestination, LogRoutingOverrides, StorageLayout,
    TieredLayoutPaths,
};

#[test]
fn standard_layout_is_default_and_derives_stable_sidecar_paths() {
    let paths = TieredLayoutPaths::new(
        Path::new("data/main.rdb"),
        StorageLayout::default(),
        LayoutOverrides::default(),
    );

    assert_eq!(StorageLayout::default(), StorageLayout::Standard);
    assert_eq!(paths.data_file, Path::new("data/main.rdb"));
    assert_eq!(paths.support_dir, Path::new("data/main.rdb.red"));
    assert_eq!(paths.wal_file, Path::new("data/main.rdb-uwal"));
    assert_eq!(
        paths.logical_wal_file,
        Path::new("data/main.rdb.logical.wal")
    );
    assert_eq!(
        paths.snapshot_dir,
        Some(Path::new("data/main.rdb.red/snapshots").to_path_buf())
    );
    assert_eq!(
        paths.index_dir,
        Some(Path::new("data/main.rdb.red/indexes").to_path_buf())
    );
    assert_eq!(paths.cache_dir, None);
    assert_eq!(paths.logs_dir, None);
    assert_eq!(paths.audit_log_destination, LogDestination::Stderr);
    assert_eq!(paths.slow_log_destination, LogDestination::Stderr);
}

#[test]
fn layout_presets_expand_to_deterministic_toggles() {
    let cases = [
        (
            StorageLayout::Minimal,
            LayoutToggles {
                dedicated_wal_dir: false,
                dedicated_index_dir: false,
                dedicated_cache_dir: false,
                dedicated_snapshot_dir: false,
                dedicated_blob_dir: false,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
        ),
        (
            StorageLayout::Standard,
            LayoutToggles {
                dedicated_wal_dir: false,
                dedicated_index_dir: true,
                dedicated_cache_dir: false,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: false,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
        ),
        (
            StorageLayout::Performance,
            LayoutToggles {
                dedicated_wal_dir: true,
                dedicated_index_dir: true,
                dedicated_cache_dir: true,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: true,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
        ),
        (
            StorageLayout::Max,
            LayoutToggles {
                dedicated_wal_dir: true,
                dedicated_index_dir: true,
                dedicated_cache_dir: true,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: true,
                dedicated_temp_dir: true,
                dedicated_metrics_dir: true,
            },
        ),
    ];

    for (layout, expected) in cases {
        assert_eq!(layout.expand(&LayoutOverrides::default()), expected);
    }
}

#[test]
fn overrides_apply_after_preset_expansion() {
    let overrides = LayoutOverrides {
        dedicated_wal_dir: Some(true),
        dedicated_index_dir: Some(false),
        dedicated_cache_dir: Some(true),
        dedicated_snapshot_dir: Some(false),
        dedicated_blob_dir: None,
        dedicated_temp_dir: Some(true),
        dedicated_metrics_dir: None,
        logs: LogRoutingOverrides::default(),
    };

    assert_eq!(
        StorageLayout::Standard.expand(&overrides),
        LayoutToggles {
            dedicated_wal_dir: true,
            dedicated_index_dir: false,
            dedicated_cache_dir: true,
            dedicated_snapshot_dir: false,
            dedicated_blob_dir: false,
            dedicated_temp_dir: true,
            dedicated_metrics_dir: false,
        }
    );
}

#[test]
fn minimal_layout_keeps_optional_tier_dirs_disabled() {
    let paths = TieredLayoutPaths::new(
        Path::new("main"),
        StorageLayout::Minimal,
        LayoutOverrides::default(),
    );

    assert_eq!(paths.support_dir, Path::new("main.red"));
    assert_eq!(paths.wal_file, Path::new("main.rdb-uwal"));
    assert_eq!(paths.logical_wal_file, Path::new("main.logical.wal"));
    assert_eq!(paths.temp_file, Path::new("main.rdb-tmp"));
    assert_eq!(paths.snapshot_dir, None);
    assert_eq!(paths.index_dir, None);
    assert_eq!(paths.cache_dir, None);
    assert_eq!(paths.blob_dir, None);
    assert_eq!(paths.metrics_dir, None);
    assert_eq!(paths.logs_dir, None);
    assert_eq!(paths.audit_log_destination, LogDestination::Stderr);
    assert_eq!(paths.slow_log_destination, LogDestination::Stderr);
    assert!(paths.dirs_to_create().is_empty());
}

#[test]
fn max_layout_places_every_tier_under_support_dir() {
    let paths = TieredLayoutPaths::new(
        Path::new("/var/lib/reddb/main.rdb"),
        StorageLayout::Max,
        LayoutOverrides::default(),
    );

    assert_eq!(
        paths.wal_file,
        Path::new("/var/lib/reddb/main.rdb.red/wal/main.rdb-uwal")
    );
    assert_eq!(
        paths.logical_wal_file,
        Path::new("/var/lib/reddb/main.rdb.red/wal/main.rdb.logical.wal")
    );
    assert_eq!(
        paths.temp_file,
        Path::new("/var/lib/reddb/main.rdb.red/tmp/main.rdb-tmp")
    );
    assert_eq!(
        paths.snapshot_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/snapshots").to_path_buf())
    );
    assert_eq!(
        paths.index_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/indexes").to_path_buf())
    );
    assert_eq!(
        paths.cache_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/cache").to_path_buf())
    );
    assert_eq!(
        paths.blob_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/blobs").to_path_buf())
    );
    assert_eq!(
        paths.metrics_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/metrics").to_path_buf())
    );
    assert_eq!(
        paths.logs_dir,
        Some(Path::new("/var/lib/reddb/main.rdb.red/logs").to_path_buf())
    );
    assert_eq!(
        paths.audit_log_destination,
        LogDestination::File(
            Path::new("/var/lib/reddb/main.rdb.red/logs/audit.log").to_path_buf()
        )
    );
    assert_eq!(
        paths.slow_log_destination,
        LogDestination::File(Path::new("/var/lib/reddb/main.rdb.red/logs/slow.log").to_path_buf())
    );
    assert_eq!(
        paths.dirs_to_create(),
        vec![
            Path::new("/var/lib/reddb").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/blobs").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/cache").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/indexes").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/logs").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/metrics").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/snapshots").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/tmp").to_path_buf(),
            Path::new("/var/lib/reddb/main.rdb.red/wal").to_path_buf(),
        ]
    );
}

#[test]
fn performance_layout_routes_audit_and_slow_logs_under_support_dir() {
    let paths = TieredLayoutPaths::new(
        Path::new("/srv/main.rdb"),
        StorageLayout::Performance,
        LayoutOverrides::default(),
    );

    assert_eq!(
        paths.audit_log_destination,
        LogDestination::File(Path::new("/srv/main.rdb.red/logs/audit.log").to_path_buf())
    );
    assert_eq!(
        paths.slow_log_destination,
        LogDestination::File(Path::new("/srv/main.rdb.red/logs/slow.log").to_path_buf())
    );
    assert_eq!(
        paths.logs_dir,
        Some(Path::new("/srv/main.rdb.red/logs").to_path_buf())
    );
    assert!(paths
        .dirs_to_create()
        .contains(&Path::new("/srv/main.rdb.red/logs").to_path_buf()));
}

#[test]
fn standard_layout_defaults_logs_to_stderr() {
    let paths = TieredLayoutPaths::new(
        Path::new("/srv/main.rdb"),
        StorageLayout::Standard,
        LayoutOverrides::default(),
    );

    assert_eq!(paths.audit_log_destination, LogDestination::Stderr);
    assert_eq!(paths.slow_log_destination, LogDestination::Stderr);
    assert_eq!(paths.logs_dir, None);
    assert_eq!(paths.audit_log_destination.describe(), "stderr");
}

#[test]
fn log_routing_overrides_replace_tier_defaults() {
    let overrides = LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(LogDestination::File(
                Path::new("/var/log/reddb/audit.log").to_path_buf(),
            )),
            slow_log: Some(LogDestination::Syslog),
        },
        ..Default::default()
    };

    let paths = TieredLayoutPaths::new(
        Path::new("/srv/main.rdb"),
        StorageLayout::Standard,
        overrides,
    );

    assert_eq!(
        paths.audit_log_destination,
        LogDestination::File(Path::new("/var/log/reddb/audit.log").to_path_buf())
    );
    assert_eq!(paths.slow_log_destination, LogDestination::Syslog);
    assert_eq!(
        paths.logs_dir,
        Some(Path::new("/srv/main.rdb.red/logs").to_path_buf())
    );
    let dirs = paths.dirs_to_create();
    assert!(dirs.contains(&Path::new("/var/log/reddb").to_path_buf()));
}

#[test]
fn override_can_force_stderr_on_performance_tier() {
    let overrides = LayoutOverrides {
        logs: LogRoutingOverrides {
            audit_log: Some(LogDestination::Stderr),
            slow_log: Some(LogDestination::Stderr),
        },
        ..Default::default()
    };

    let paths = TieredLayoutPaths::new(
        Path::new("/srv/main.rdb"),
        StorageLayout::Performance,
        overrides,
    );

    assert_eq!(paths.audit_log_destination, LogDestination::Stderr);
    assert_eq!(paths.slow_log_destination, LogDestination::Stderr);
    assert_eq!(paths.logs_dir, None);
}

#[test]
fn serde_config_defaults_to_standard_and_accepts_overrides() {
    #[derive(Debug, Default, Deserialize)]
    struct Config {
        #[serde(default)]
        layout: StorageLayout,
        #[serde(default)]
        overrides: LayoutOverrides,
    }

    let default_cfg: Config = toml::from_str("").expect("empty config uses defaults");
    assert_eq!(default_cfg.layout, StorageLayout::Standard);
    assert_eq!(default_cfg.overrides, LayoutOverrides::default());

    let cfg: Config = toml::from_str(
        r#"
layout = "performance"

[overrides]
dedicated_wal_dir = false
dedicated_metrics_dir = true
"#,
    )
    .expect("layout config parses");

    let toggles = cfg.layout.expand(&cfg.overrides);
    assert!(!toggles.dedicated_wal_dir);
    assert!(toggles.dedicated_metrics_dir);
    assert!(toggles.dedicated_cache_dir);
}
