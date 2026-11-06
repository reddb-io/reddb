use reddb_server::{
    api::{CatalogSnapshot, CollectionStats, RedDBOptions},
    physical::PhysicalMetadataFile,
};
use std::collections::BTreeMap;

#[test]
fn binary_catalog_collision_is_not_hidden_by_missing_json_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.rdb");
    let mut metadata = PhysicalMetadataFile::from_state(
        RedDBOptions::persistent(&path),
        CatalogSnapshot::default(),
        BTreeMap::new(),
        Vec::new(),
        None,
    );
    for name in ["red_config", "red.config"] {
        metadata
            .catalog
            .stats_by_collection
            .insert(name.into(), CollectionStats::default());
    }
    metadata
        .save_to_binary_path(&PhysicalMetadataFile::metadata_binary_path_for(&path))
        .unwrap();
    let expected = PhysicalMetadataFile::load_from_binary_path(
        &PhysicalMetadataFile::metadata_binary_path_for(&path),
    )
    .unwrap_err()
    .to_string();
    assert!(expected.contains("both legacy collection"));
    let actual = PhysicalMetadataFile::inspect_for_data_path(&path)
        .unwrap_err()
        .to_string();
    assert_eq!(actual, expected);
    assert!(!PhysicalMetadataFile::metadata_path_for(&path).exists());
}
