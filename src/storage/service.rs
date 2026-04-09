use crate::storage::client::{PersistenceConfig, PersistenceManager, QueryManager};
use crate::storage::layout::SegmentKind;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

/// Identifies a logical partition in the storage engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PartitionKey {
    Domain(String),
    Target(String),
    Date(u32),
    Custom(String),
}

/// Metadata describing a partition's layout on disk.
#[derive(Debug, Clone)]
pub struct PartitionMetadata {
    pub key: PartitionKey,
    pub label: String,
    pub storage_path: PathBuf,
    pub segments: Vec<SegmentKind>,
    pub attributes: BTreeMap<String, String>,
    pub last_refreshed: Option<SystemTime>,
}

impl PartitionMetadata {
    pub fn new<K: Into<PartitionKey>, L: Into<String>, P: Into<PathBuf>>(
        key: K,
        label: L,
        storage_path: P,
        segments: Vec<SegmentKind>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            storage_path: storage_path.into(),
            segments,
            attributes: BTreeMap::new(),
            last_refreshed: None,
        }
    }

    pub fn with_attribute<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub fn with_attributes<I, K, V>(mut self, attrs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (key, value) in attrs.into_iter() {
            self.attributes.insert(key.into(), value.into());
        }
        self
    }

    pub fn with_last_refreshed(mut self, timestamp: SystemTime) -> Self {
        self.last_refreshed = Some(timestamp);
        self
    }
}

#[derive(Default)]
struct PartitionRegistry {
    entries: Vec<PartitionMetadata>,
    index: HashMap<PartitionKey, usize>,
}

impl PartitionRegistry {
    fn new() -> Self {
        Self::default()
    }

    fn upsert(&mut self, mut meta: PartitionMetadata) {
        if let Some(&idx) = self.index.get(&meta.key) {
            let entry = &mut self.entries[idx];
            let mut merged_attributes = entry.attributes.clone();
            if !meta.attributes.is_empty() {
                for (key, value) in meta.attributes.iter() {
                    merged_attributes.insert(key.clone(), value.clone());
                }
                meta.attributes = merged_attributes;
            } else {
                meta.attributes = entry.attributes.clone();
            }

            if meta.last_refreshed.is_none() {
                meta.last_refreshed = entry.last_refreshed;
            }

            *entry = meta;
        } else {
            let idx = self.entries.len();
            self.index.insert(meta.key.clone(), idx);
            self.entries.push(meta);
        }
    }

    fn snapshot(&self) -> Vec<PartitionMetadata> {
        self.entries.clone()
    }

    fn filter<F>(&self, predicate: F) -> Vec<PartitionMetadata>
    where
        F: Fn(&PartitionMetadata) -> bool,
    {
        self.entries
            .iter()
            .filter(|meta| predicate(meta))
            .cloned()
            .collect()
    }

    fn get(&self, key: &PartitionKey) -> Option<PartitionMetadata> {
        self.index
            .get(key)
            .and_then(|&idx| self.entries.get(idx))
            .cloned()
    }

    fn merge_attributes(&mut self, key: &PartitionKey, attributes: Vec<(String, String)>) {
        if attributes.is_empty() {
            return;
        }
        if let Some(&idx) = self.index.get(key) {
            let entry = &mut self.entries[idx];
            for (attr_key, value) in attributes {
                entry.attributes.insert(attr_key, value);
            }
            entry.last_refreshed.get_or_insert(SystemTime::now());
        }
    }
}

/// Central façade that coordinates persistence/query managers and partition metadata.
pub struct StorageService {
    partitions: RwLock<PartitionRegistry>,
}

const DEFAULT_SEGMENTS: &[SegmentKind] = &[
    SegmentKind::Ports,
    SegmentKind::Subdomains,
    SegmentKind::Whois,
    SegmentKind::Tls,
    SegmentKind::Dns,
    SegmentKind::Http,
    SegmentKind::Host,
];

impl StorageService {
    fn new() -> Self {
        Self {
            partitions: RwLock::new(PartitionRegistry::new()),
        }
    }

    /// Access the global storage service instance.
    pub fn global() -> &'static StorageService {
        static INSTANCE: OnceLock<StorageService> = OnceLock::new();
        INSTANCE.get_or_init(StorageService::new)
    }

    /// Register or update a partition. Future persistence operations can use this metadata
    /// to route writes/reads without crawling the directory structure.
    pub fn register_partition(&self, mut metadata: PartitionMetadata) {
        if metadata.last_refreshed.is_none() {
            metadata.last_refreshed = Some(SystemTime::now());
        }
        let mut guard = self.partitions.write().expect("partition lock poisoned");
        guard.upsert(metadata);
    }

    /// Annotate an existing partition with additional attributes.
    pub fn annotate_partition<I>(&self, key: &PartitionKey, attrs: I)
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let attributes: Vec<(String, String)> = attrs.into_iter().collect();
        if attributes.is_empty() {
            return;
        }
        let mut guard = self.partitions.write().expect("partition lock poisoned");
        guard.merge_attributes(key, attributes);
    }

    /// Convenience helper: register the standard per-target partition.
    pub fn ensure_target_partition<P: Into<PathBuf>>(
        &self,
        target: &str,
        path: P,
        segments: Option<Vec<SegmentKind>>,
        attributes: Option<Vec<(String, String)>>,
    ) {
        let segments_vec = segments.unwrap_or_else(|| DEFAULT_SEGMENTS.to_vec());
        let mut metadata = PartitionMetadata::new(
            PartitionKey::Target(target.to_string()),
            format!("target:{}", target),
            path.into(),
            segments_vec,
        )
        .with_attribute("category", "target")
        .with_attribute("target", target);

        if let Some(attrs) = attributes {
            metadata = metadata.with_attributes(attrs);
        }

        self.register_partition(metadata);
    }

    /// Refresh an existing target partition by inspecting the on-disk segments.
    pub fn refresh_target_partition<P: AsRef<Path>>(
        &self,
        target: &str,
        path: P,
    ) -> io::Result<()> {
        self.refresh_partition(
            PartitionKey::Target(target.to_string()),
            format!("target:{}", target),
            path,
        )
    }

    pub fn refresh_partition<P: AsRef<Path>>(
        &self,
        key: PartitionKey,
        label: String,
        path: P,
    ) -> io::Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(());
        }

        match Self::inspect_segments(path) {
            Ok(segments) => {
                let mut metadata =
                    PartitionMetadata::new(key.clone(), label, path.to_path_buf(), segments);

                if let Some(existing) = self.partition(&key) {
                    let attrs = existing
                        .attributes
                        .iter()
                        .map(|(attr_key, value)| (attr_key.clone(), value.clone()))
                        .collect::<Vec<_>>();
                    metadata = metadata.with_attributes(attrs);
                }

                self.register_partition(metadata);
                Ok(())
            }
            Err(err) => Err(io::Error::new(
                err.kind(),
                format!("{}: {}", path.display(), err),
            )),
        }
    }

    /// Return a cloned snapshot of all known partitions.
    pub fn partitions(&self) -> Vec<PartitionMetadata> {
        let guard = self.partitions.read().expect("partition lock poisoned");
        guard.snapshot()
    }

    /// Return partitions matching a predicate.
    pub fn partitions_filtered<F>(&self, predicate: F) -> Vec<PartitionMetadata>
    where
        F: Fn(&PartitionMetadata) -> bool,
    {
        let guard = self.partitions.read().expect("partition lock poisoned");
        guard.filter(predicate)
    }

    /// Return partitions that contain a given segment.
    pub fn partitions_with_segment(&self, segment: SegmentKind) -> Vec<PartitionMetadata> {
        self.partitions_filtered(|meta| meta.segments.contains(&segment))
    }

    /// Return partitions matching an attribute.
    pub fn partitions_with_attribute(&self, key: &str, value: &str) -> Vec<PartitionMetadata> {
        self.partitions_filtered(|meta| {
            meta.attributes
                .get(key)
                .map(|candidate| candidate == value)
                .unwrap_or(false)
        })
    }

    /// Look up a partition by key.
    pub fn partition(&self, key: &PartitionKey) -> Option<PartitionMetadata> {
        let guard = self.partitions.read().expect("partition lock poisoned");
        guard.get(key)
    }

    /// Create a persistence manager for a given target.
    pub fn persistence_for_target(
        &self,
        target: &str,
        persist: Option<bool>,
    ) -> Result<PersistenceManager, String> {
        self.persistence_for_target_with(target, persist, None, Vec::<(String, String)>::new())
    }

    /// Create a persistence manager with explicit segments and metadata attributes.
    pub fn persistence_for_target_with<I>(
        &self,
        target: &str,
        persist: Option<bool>,
        segments: Option<Vec<SegmentKind>>,
        attrs: I,
    ) -> Result<PersistenceManager, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let manager = PersistenceManager::new(target, persist)?;
        let attrs_vec: Vec<(String, String)> = attrs.into_iter().collect();
        if let Some(path) = manager.db_path().cloned() {
            self.ensure_target_partition(target, path, segments, Some(attrs_vec));
        }
        Ok(manager)
    }

    /// Create a persistence manager using a PersistenceConfig (supports --save, --db-password flags).
    pub fn persistence_with_config<I>(
        &self,
        target: &str,
        config: PersistenceConfig,
        attrs: I,
    ) -> Result<PersistenceManager, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let manager = PersistenceManager::with_config(target, config)?;
        let attrs_vec: Vec<(String, String)> = attrs.into_iter().collect();
        if let Some(path) = manager.db_path().cloned() {
            self.ensure_target_partition(target, path, None, Some(attrs_vec));
        }
        Ok(manager)
    }

    /// Helper to build a custom partition key for arbitrary storage paths.
    pub fn key_for_path<P: AsRef<Path>>(path: P) -> PartitionKey {
        PartitionKey::Custom(path.as_ref().display().to_string())
    }

    /// Open a query manager for the provided .rdb path.
    pub fn open_query_manager<P: Into<PathBuf>>(&self, path: P) -> std::io::Result<QueryManager> {
        let path_buf = path.into();
        let key = Self::key_for_path(&path_buf);
        if let Err(err) = self.refresh_partition(
            key.clone(),
            format!("custom:{}", path_buf.display()),
            &path_buf,
        ) {
            // Ignore refresh errors for ad-hoc paths
            let _ = err;
        }
        QueryManager::open(path_buf)
    }

    /// Resolve the standard database path for a target
    pub fn db_path(target: &str) -> PathBuf {
        // Keep the same naming convention as PersistenceManager.

        if target.ends_with(".json") {
            PathBuf::from(target)
        } else {
            PathBuf::from(format!("{}.json", target))
        }
    }

    fn inspect_segments(path: &Path) -> io::Result<Vec<SegmentKind>> {
        let db = crate::storage::RedDB::open(path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut segments = Vec::new();
        for collection in db.collections() {
            let kind = match collection.as_str() {
                "ports" => Some(SegmentKind::Ports),
                "domains" => Some(SegmentKind::Subdomains),
                "dns" => Some(SegmentKind::Dns),
                "http" => Some(SegmentKind::Http),
                "tls" => Some(SegmentKind::Tls),
                "whois" => Some(SegmentKind::Whois),
                "hosts" => Some(SegmentKind::Host),
                "proxy" => Some(SegmentKind::Proxy),
                "mitre" => Some(SegmentKind::Mitre),
                "iocs" => Some(SegmentKind::Ioc),
                "vulns" => Some(SegmentKind::Vuln),
                "sessions" => Some(SegmentKind::Sessions),
                "playbooks" => Some(SegmentKind::Playbooks),
                "actions" => Some(SegmentKind::Actions),
                "traces" => Some(SegmentKind::Traces),
                "loot" => Some(SegmentKind::Loot),
                _ => None,
            };
            if let Some(kind) = kind {
                if !segments.contains(&kind) {
                    segments.push(kind);
                }
            }
        }
        Ok(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::RedDB;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_db(name: &str) -> (FileGuard, PathBuf) {
        let path = std::env::temp_dir().join(format!("rb_svc_{}_{}.db", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    // ==================== PartitionKey Tests ====================

    #[test]
    fn test_partition_key_domain() {
        let key = PartitionKey::Domain("example.com".to_string());
        assert_eq!(key, PartitionKey::Domain("example.com".to_string()));
    }

    #[test]
    fn test_partition_key_target() {
        let key = PartitionKey::Target("192.168.1.1".to_string());
        assert_eq!(key, PartitionKey::Target("192.168.1.1".to_string()));
    }

    #[test]
    fn test_partition_key_date() {
        let key = PartitionKey::Date(20231215);
        assert_eq!(key, PartitionKey::Date(20231215));
    }

    #[test]
    fn test_partition_key_custom() {
        let key = PartitionKey::Custom("/custom/path".to_string());
        assert_eq!(key, PartitionKey::Custom("/custom/path".to_string()));
    }

    #[test]
    fn test_partition_key_inequality() {
        let key1 = PartitionKey::Domain("a.com".to_string());
        let key2 = PartitionKey::Domain("b.com".to_string());
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_partition_key_type_inequality() {
        let key1 = PartitionKey::Domain("example.com".to_string());
        let key2 = PartitionKey::Target("example.com".to_string());
        assert_ne!(key1, key2);
    }

    // ==================== PartitionMetadata Tests ====================

    #[test]
    fn test_partition_metadata_new() {
        let meta = PartitionMetadata::new(
            PartitionKey::Target("test".to_string()),
            "test_label",
            "/path/to/db",
            vec![SegmentKind::Ports],
        );

        assert_eq!(meta.key, PartitionKey::Target("test".to_string()));
        assert_eq!(meta.label, "test_label");
        assert_eq!(meta.storage_path, PathBuf::from("/path/to/db"));
        assert_eq!(meta.segments.len(), 1);
        assert!(meta.attributes.is_empty());
        assert!(meta.last_refreshed.is_none());
    }

    #[test]
    fn test_partition_metadata_with_attribute() {
        let meta = PartitionMetadata::new(
            PartitionKey::Domain("test.com".to_string()),
            "label",
            "/path",
            vec![],
        )
        .with_attribute("key1", "value1");

        assert_eq!(meta.attributes.get("key1"), Some(&"value1".to_string()));
    }

    #[test]
    fn test_partition_metadata_with_multiple_attributes() {
        let meta = PartitionMetadata::new(
            PartitionKey::Domain("test.com".to_string()),
            "label",
            "/path",
            vec![],
        )
        .with_attribute("key1", "value1")
        .with_attribute("key2", "value2");

        assert_eq!(meta.attributes.len(), 2);
        assert_eq!(meta.attributes.get("key1"), Some(&"value1".to_string()));
        assert_eq!(meta.attributes.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_partition_metadata_with_attributes_batch() {
        let attrs = vec![
            ("category", "scan"),
            ("target", "192.168.1.1"),
            ("protocol", "tcp"),
        ];

        let meta = PartitionMetadata::new(
            PartitionKey::Target("host".to_string()),
            "label",
            "/path",
            vec![],
        )
        .with_attributes(attrs);

        assert_eq!(meta.attributes.len(), 3);
        assert_eq!(meta.attributes.get("category"), Some(&"scan".to_string()));
        assert_eq!(
            meta.attributes.get("target"),
            Some(&"192.168.1.1".to_string())
        );
    }

    #[test]
    fn test_partition_metadata_with_last_refreshed() {
        let now = SystemTime::now();
        let meta = PartitionMetadata::new(
            PartitionKey::Domain("test.com".to_string()),
            "label",
            "/path",
            vec![],
        )
        .with_last_refreshed(now);

        assert_eq!(meta.last_refreshed, Some(now));
    }

    #[test]
    fn test_partition_metadata_multiple_segments() {
        let segments = vec![
            SegmentKind::Ports,
            SegmentKind::Subdomains,
            SegmentKind::Dns,
            SegmentKind::Http,
            SegmentKind::Tls,
        ];

        let meta = PartitionMetadata::new(
            PartitionKey::Target("test".to_string()),
            "label",
            "/path",
            segments.clone(),
        );

        assert_eq!(meta.segments.len(), 5);
        assert!(meta.segments.contains(&SegmentKind::Ports));
        assert!(meta.segments.contains(&SegmentKind::Tls));
    }

    // ==================== PartitionRegistry Tests ====================

    #[test]
    fn test_partition_registry_new() {
        let registry = PartitionRegistry::new();
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn test_partition_registry_upsert_new() {
        let mut registry = PartitionRegistry::new();
        let meta = PartitionMetadata::new(
            PartitionKey::Target("test".to_string()),
            "label",
            "/path",
            vec![],
        );

        registry.upsert(meta);

        assert_eq!(registry.snapshot().len(), 1);
    }

    #[test]
    fn test_partition_registry_upsert_update() {
        let mut registry = PartitionRegistry::new();
        let key = PartitionKey::Target("test".to_string());

        let meta1 = PartitionMetadata::new(key.clone(), "label1", "/path1", vec![]);
        registry.upsert(meta1);

        let meta2 = PartitionMetadata::new(key.clone(), "label2", "/path2", vec![]);
        registry.upsert(meta2);

        // Should still have only 1 entry (updated)
        assert_eq!(registry.snapshot().len(), 1);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot[0].label, "label2");
        assert_eq!(snapshot[0].storage_path, PathBuf::from("/path2"));
    }

    #[test]
    fn test_partition_registry_get() {
        let mut registry = PartitionRegistry::new();
        let key = PartitionKey::Domain("example.com".to_string());

        registry.upsert(PartitionMetadata::new(
            key.clone(),
            "example",
            "/path/example",
            vec![SegmentKind::Ports],
        ));

        let result = registry.get(&key);
        assert!(result.is_some());
        assert_eq!(result.unwrap().label, "example");
    }

    #[test]
    fn test_partition_registry_get_nonexistent() {
        let registry = PartitionRegistry::new();
        let key = PartitionKey::Domain("nonexistent.com".to_string());

        assert!(registry.get(&key).is_none());
    }

    #[test]
    fn test_partition_registry_filter() {
        let mut registry = PartitionRegistry::new();

        registry.upsert(
            PartitionMetadata::new(
                PartitionKey::Target("target1".to_string()),
                "target1",
                "/p1",
                vec![SegmentKind::Ports],
            )
            .with_attribute("category", "scan"),
        );

        registry.upsert(
            PartitionMetadata::new(
                PartitionKey::Target("target2".to_string()),
                "target2",
                "/p2",
                vec![SegmentKind::Dns],
            )
            .with_attribute("category", "recon"),
        );

        registry.upsert(
            PartitionMetadata::new(
                PartitionKey::Target("target3".to_string()),
                "target3",
                "/p3",
                vec![SegmentKind::Ports, SegmentKind::Dns],
            )
            .with_attribute("category", "scan"),
        );

        let scan_partitions =
            registry.filter(|meta| meta.attributes.get("category") == Some(&"scan".to_string()));

        assert_eq!(scan_partitions.len(), 2);
    }

    #[test]
    fn test_partition_registry_merge_attributes() {
        let mut registry = PartitionRegistry::new();
        let key = PartitionKey::Target("test".to_string());

        registry.upsert(
            PartitionMetadata::new(key.clone(), "label", "/path", vec![])
                .with_attribute("initial", "value"),
        );

        registry.merge_attributes(
            &key,
            vec![
                ("new_key".to_string(), "new_value".to_string()),
                ("another".to_string(), "attr".to_string()),
            ],
        );

        let meta = registry.get(&key).unwrap();
        assert_eq!(meta.attributes.len(), 3);
        assert_eq!(meta.attributes.get("initial"), Some(&"value".to_string()));
        assert_eq!(
            meta.attributes.get("new_key"),
            Some(&"new_value".to_string())
        );
        assert!(meta.last_refreshed.is_some());
    }

    #[test]
    fn test_partition_registry_merge_attributes_empty() {
        let mut registry = PartitionRegistry::new();
        let key = PartitionKey::Target("test".to_string());

        registry.upsert(PartitionMetadata::new(
            key.clone(),
            "label",
            "/path",
            vec![],
        ));

        // Empty merge should not change anything
        registry.merge_attributes(&key, vec![]);

        let meta = registry.get(&key).unwrap();
        assert!(meta.attributes.is_empty());
    }

    #[test]
    fn test_partition_registry_upsert_preserves_attributes() {
        let mut registry = PartitionRegistry::new();
        let key = PartitionKey::Target("test".to_string());

        registry.upsert(
            PartitionMetadata::new(key.clone(), "label", "/path", vec![])
                .with_attribute("preserved", "yes"),
        );

        // Upsert with empty attributes should preserve existing
        registry.upsert(PartitionMetadata::new(
            key.clone(),
            "new_label",
            "/new_path",
            vec![],
        ));

        let meta = registry.get(&key).unwrap();
        assert_eq!(meta.attributes.get("preserved"), Some(&"yes".to_string()));
    }

    // ==================== StorageService Tests ====================

    #[test]
    fn test_storage_service_global() {
        let service1 = StorageService::global();
        let service2 = StorageService::global();

        // Should be the same instance
        assert!(std::ptr::eq(service1, service2));
    }

    #[test]
    fn test_storage_service_key_for_path() {
        let key = StorageService::key_for_path("/some/path/file.db");
        assert_eq!(key, PartitionKey::Custom("/some/path/file.db".to_string()));
    }

    #[test]
    fn test_storage_service_partitions_empty() {
        let service = StorageService::new();
        assert!(service.partitions().is_empty());
    }

    #[test]
    fn test_storage_service_register_partition() {
        let service = StorageService::new();
        let meta = PartitionMetadata::new(
            PartitionKey::Target("test".to_string()),
            "test",
            "/test/path",
            vec![SegmentKind::Ports],
        );

        service.register_partition(meta);

        let partitions = service.partitions();
        assert_eq!(partitions.len(), 1);
        assert!(partitions[0].last_refreshed.is_some());
    }

    #[test]
    fn test_storage_service_partition_lookup() {
        let service = StorageService::new();
        let key = PartitionKey::Target("test".to_string());

        service.register_partition(PartitionMetadata::new(
            key.clone(),
            "test",
            "/test/path",
            vec![],
        ));

        let result = service.partition(&key);
        assert!(result.is_some());
        assert_eq!(result.unwrap().label, "test");
    }

    #[test]
    fn test_storage_service_partition_lookup_nonexistent() {
        let service = StorageService::new();
        let key = PartitionKey::Target("nonexistent".to_string());

        assert!(service.partition(&key).is_none());
    }

    #[test]
    fn test_storage_service_annotate_partition() {
        let service = StorageService::new();
        let key = PartitionKey::Target("test".to_string());

        service.register_partition(PartitionMetadata::new(key.clone(), "test", "/path", vec![]));

        service.annotate_partition(&key, vec![("annotation".to_string(), "value".to_string())]);

        let meta = service.partition(&key).unwrap();
        assert_eq!(
            meta.attributes.get("annotation"),
            Some(&"value".to_string())
        );
    }

    #[test]
    fn test_storage_service_annotate_nonexistent() {
        let service = StorageService::new();
        let key = PartitionKey::Target("nonexistent".to_string());

        // Should not panic
        service.annotate_partition(&key, vec![("key".to_string(), "value".to_string())]);

        // Should still be none
        assert!(service.partition(&key).is_none());
    }

    #[test]
    fn test_storage_service_ensure_target_partition() {
        let service = StorageService::new();
        let target = "192.168.1.1";

        service.ensure_target_partition(target, "/path/to/db", None, None);

        let key = PartitionKey::Target(target.to_string());
        let meta = service.partition(&key).unwrap();

        assert_eq!(meta.label, format!("target:{}", target));
        assert_eq!(meta.attributes.get("category"), Some(&"target".to_string()));
        assert_eq!(meta.attributes.get("target"), Some(&target.to_string()));
        assert!(!meta.segments.is_empty()); // Default segments
    }

    #[test]
    fn test_storage_service_ensure_target_partition_custom_segments() {
        let service = StorageService::new();
        let target = "test.com";

        service.ensure_target_partition(
            target,
            "/path",
            Some(vec![SegmentKind::Ports, SegmentKind::Tls]),
            None,
        );

        let key = PartitionKey::Target(target.to_string());
        let meta = service.partition(&key).unwrap();

        assert_eq!(meta.segments.len(), 2);
        assert!(meta.segments.contains(&SegmentKind::Ports));
        assert!(meta.segments.contains(&SegmentKind::Tls));
    }

    #[test]
    fn test_storage_service_ensure_target_partition_with_attrs() {
        let service = StorageService::new();
        let target = "test.com";

        service.ensure_target_partition(
            target,
            "/path",
            None,
            Some(vec![
                ("custom".to_string(), "attr".to_string()),
                ("scan_type".to_string(), "full".to_string()),
            ]),
        );

        let key = PartitionKey::Target(target.to_string());
        let meta = service.partition(&key).unwrap();

        assert_eq!(meta.attributes.get("custom"), Some(&"attr".to_string()));
        assert_eq!(meta.attributes.get("scan_type"), Some(&"full".to_string()));
    }

    #[test]
    fn test_storage_service_partitions_filtered() {
        let service = StorageService::new();

        service.register_partition(
            PartitionMetadata::new(
                PartitionKey::Target("t1".to_string()),
                "t1",
                "/t1",
                vec![SegmentKind::Ports],
            )
            .with_attribute("type", "scan"),
        );

        service.register_partition(
            PartitionMetadata::new(
                PartitionKey::Target("t2".to_string()),
                "t2",
                "/t2",
                vec![SegmentKind::Dns],
            )
            .with_attribute("type", "recon"),
        );

        let scan_partitions = service
            .partitions_filtered(|meta| meta.attributes.get("type") == Some(&"scan".to_string()));

        assert_eq!(scan_partitions.len(), 1);
        assert_eq!(scan_partitions[0].label, "t1");
    }

    #[test]
    fn test_storage_service_partitions_with_segment() {
        let service = StorageService::new();

        service.register_partition(PartitionMetadata::new(
            PartitionKey::Target("t1".to_string()),
            "t1",
            "/t1",
            vec![SegmentKind::Ports, SegmentKind::Dns],
        ));

        service.register_partition(PartitionMetadata::new(
            PartitionKey::Target("t2".to_string()),
            "t2",
            "/t2",
            vec![SegmentKind::Dns, SegmentKind::Tls],
        ));

        service.register_partition(PartitionMetadata::new(
            PartitionKey::Target("t3".to_string()),
            "t3",
            "/t3",
            vec![SegmentKind::Ports],
        ));

        let dns_partitions = service.partitions_with_segment(SegmentKind::Dns);
        assert_eq!(dns_partitions.len(), 2);

        let ports_partitions = service.partitions_with_segment(SegmentKind::Ports);
        assert_eq!(ports_partitions.len(), 2);

        let tls_partitions = service.partitions_with_segment(SegmentKind::Tls);
        assert_eq!(tls_partitions.len(), 1);
    }

    #[test]
    fn test_storage_service_partitions_with_attribute() {
        let service = StorageService::new();

        service.register_partition(
            PartitionMetadata::new(PartitionKey::Target("t1".to_string()), "t1", "/t1", vec![])
                .with_attribute("env", "prod"),
        );

        service.register_partition(
            PartitionMetadata::new(PartitionKey::Target("t2".to_string()), "t2", "/t2", vec![])
                .with_attribute("env", "dev"),
        );

        service.register_partition(
            PartitionMetadata::new(PartitionKey::Target("t3".to_string()), "t3", "/t3", vec![])
                .with_attribute("env", "prod"),
        );

        let prod_partitions = service.partitions_with_attribute("env", "prod");
        assert_eq!(prod_partitions.len(), 2);

        let dev_partitions = service.partitions_with_attribute("env", "dev");
        assert_eq!(dev_partitions.len(), 1);

        let staging_partitions = service.partitions_with_attribute("env", "staging");
        assert!(staging_partitions.is_empty());
    }

    #[test]
    fn test_storage_service_refresh_nonexistent_path() {
        let service = StorageService::new();

        let result = service.refresh_partition(
            PartitionKey::Custom("test".to_string()),
            "test".to_string(),
            "/nonexistent/path/file.db",
        );

        // Should succeed (no-op for nonexistent files)
        assert!(result.is_ok());
    }

    #[test]
    #[ignore = "requires RUST_MIN_STACK=8388608"]
    fn test_storage_service_refresh_target_partition() {
        let (_guard, path) = temp_db("refresh");

        // Create a real database file
        {
            let db = RedDB::open(&path).unwrap();
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
            db.node("ports", "Port")
                .property("ip", ip.to_string())
                .property("port", 80i64)
                .property("state", "open")
                .property("service_id", 0i64)
                .property("timestamp", 0i64)
                .save()
                .unwrap();
            db.flush().unwrap();
        }

        let service = StorageService::new();
        let result = service.refresh_target_partition("test", &path);

        assert!(result.is_ok());

        let key = PartitionKey::Target("test".to_string());
        let meta = service.partition(&key);
        assert!(meta.is_some());
        // Should have detected the Ports segment
        assert!(!meta.unwrap().segments.is_empty());
    }

    #[test]
    #[ignore = "requires RUST_MIN_STACK=8388608"]
    fn test_storage_service_inspect_segments() {
        let (_guard, path) = temp_db("inspect");

        // Create database with various segments
        {
            let db = RedDB::open(&path).unwrap();
            let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
            db.node("ports", "Port")
                .property("ip", ip.to_string())
                .property("port", 22i64)
                .property("state", "open")
                .property("service_id", 0i64)
                .property("timestamp", 0i64)
                .save()
                .unwrap();
            db.node("ports", "Port")
                .property("ip", ip.to_string())
                .property("port", 80i64)
                .property("state", "open")
                .property("service_id", 0i64)
                .property("timestamp", 0i64)
                .save()
                .unwrap();
            db.flush().unwrap();
        }

        let segments = StorageService::inspect_segments(&path).unwrap();

        // Should detect ports segment
        assert!(segments.contains(&SegmentKind::Ports));
    }

    #[test]
    fn test_default_segments() {
        // Verify DEFAULT_SEGMENTS contains all expected segment types
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Ports));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Subdomains));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Whois));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Tls));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Dns));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Http));
        assert!(DEFAULT_SEGMENTS.contains(&SegmentKind::Host));
    }
}
