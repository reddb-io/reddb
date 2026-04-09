//! Engine layer facade.
//!
//! This module keeps the physical storage concerns separated from unified domain APIs.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{
    CatalogService, CatalogSnapshot, CollectionStats, RedDBError, RedDBOptions, RedDBResult,
    StorageMode, StorageMode::Persistent,
};
use crate::health::{storage_file_health, HealthProvider, HealthReport};
use crate::index::IndexCatalog;
use crate::physical::PhysicalLayout;
use crate::storage;
use crate::storage::wal::CheckpointResult;

#[derive(Debug, Clone, Copy)]
pub struct EngineStats {
    pub page_count: u32,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub file_size_bytes: Option<u64>,
    pub path_exists: bool,
}

#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub path: Option<PathBuf>,
    pub started_at_unix_ms: u128,
    pub read_only: bool,
    pub mode: StorageMode,
    pub layout: PhysicalLayout,
    pub options: RedDBOptions,
}

pub struct RedDBEngine {
    options: RedDBOptions,
    layout: PhysicalLayout,
    db: Option<storage::engine::Database>,
    indices: IndexCatalog,
    started_at_unix_ms: u128,
}

impl RedDBEngine {
    pub fn open<P: AsRef<Path>>(path: P) -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::persistent(path.as_ref()))
    }

    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let layout = PhysicalLayout::from_options(&options);

        let indices = IndexCatalog::register_default_vector_graph(
            options.has_capability(crate::api::Capability::Table),
            options.has_capability(crate::api::Capability::Graph),
        );

        let db = match options.mode {
            StorageMode::InMemory => None,
            Persistent => {
                let path = options.resolved_path("data.rdb");
                let mut config = storage::engine::DatabaseConfig::default();
                config.read_only = options.read_only;
                config.create = options.create_if_missing;
                config.verify_checksums = options.verify_checksums;
                config.auto_checkpoint_threshold = options.auto_checkpoint_pages;
                Some(storage::engine::Database::open_with_config(path, config)?)
            }
        };

        Ok(Self {
            options,
            layout,
            db,
            indices,
            started_at_unix_ms,
        })
    }

    pub fn options(&self) -> &RedDBOptions {
        &self.options
    }

    pub fn layout(&self) -> &PhysicalLayout {
        &self.layout
    }

    pub fn mode(&self) -> StorageMode {
        self.options.mode
    }

    pub fn path(&self) -> Option<&Path> {
        self.db.as_ref().map(|db| db.path())
    }

    pub fn begin_transaction(&self) -> RedDBResult<storage::wal::Transaction> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| RedDBError::InvalidConfig("in-memory mode".to_string()))?;
        Ok(db.begin()?)
    }

    pub fn sync(&self) -> RedDBResult<()> {
        if let Some(db) = &self.db {
            db.sync()?;
        }
        Ok(())
    }

    pub fn checkpoint(&self) -> RedDBResult<Option<CheckpointResult>> {
        match &self.db {
            Some(db) => db.checkpoint().map(Some).map_err(RedDBError::from),
            None => Ok(None),
        }
    }

    pub fn checkpoint_if_needed(&self) -> RedDBResult<Option<CheckpointResult>> {
        match &self.db {
            Some(db) => db.maybe_auto_checkpoint().map_err(RedDBError::from),
            None => Ok(None),
        }
    }

    pub fn stats(&self) -> EngineStats {
        let mut stats = EngineStats {
            page_count: 0,
            cache_hits: 0,
            cache_misses: 0,
            file_size_bytes: None,
            path_exists: false,
        };

        if let Some(db) = &self.db {
            stats.page_count = db.page_count();
            if let Ok(file_size) = db.file_size() {
                stats.file_size_bytes = Some(file_size);
            }
            let cache = db.cache_stats();
            stats.cache_hits = cache.hits;
            stats.cache_misses = cache.misses;
            stats.path_exists = db.path().exists();
        }

        stats
    }

    pub fn indices(&self) -> &IndexCatalog {
        &self.indices
    }

    pub fn close(self) -> RedDBResult<()> {
        if let Some(db) = self.db {
            db.close().map_err(RedDBError::from)?;
        }
        Ok(())
    }

    pub fn info(&self) -> EngineInfo {
        EngineInfo {
            path: self.path().map(Path::to_path_buf),
            started_at_unix_ms: self.started_at_unix_ms,
            read_only: self.options.read_only,
            mode: self.options.mode,
            layout: self.layout.clone(),
            options: self.options.clone(),
        }
    }

    fn fallback_snapshot(&self) -> CatalogSnapshot {
        CatalogSnapshot {
            name: "reddb_engine".into(),
            total_entities: 0,
            total_collections: 0,
            stats_by_collection: std::collections::BTreeMap::new(),
            updated_at: SystemTime::UNIX_EPOCH,
        }
    }
}

impl CatalogService for RedDBEngine {
    fn list_collections(&self) -> Vec<String> {
        Vec::new()
    }

    fn collection_stats(&self, _: &str) -> Option<CollectionStats> {
        None
    }

    fn catalog_snapshot(&self) -> CatalogSnapshot {
        self.fallback_snapshot()
    }
}

impl HealthProvider for RedDBEngine {
    fn health(&self) -> HealthReport {
        let path = self.path();
        let mut report = match path {
            Some(path) => storage_file_health(path),
            None => HealthReport::healthy().with_diagnostic("mode", "in-memory"),
        };
        report = report.with_diagnostic("engine-started-at", self.started_at_unix_ms.to_string());
        report = report.with_diagnostic("read-only", self.options.read_only.to_string());
        report.with_diagnostic("persistent", self.layout.is_persistent().to_string())
    }
}

impl crate::api::DataOps for RedDBEngine {
    fn execute_query(&self, _query: &str) -> RedDBResult<()> {
        Ok(())
    }
}

impl crate::api::QueryPlanner for RedDBEngine {
    fn plan_cost(&self, query: &str) -> Option<f64> {
        if query.trim().is_empty() {
            None
        } else {
            Some(query.len() as f64 * 0.25)
        }
    }
}
