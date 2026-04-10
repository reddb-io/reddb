//! Index layer contracts and in-memory index catalogue.
//!
//! This is a thin, stable abstraction layer above the concrete index
//! implementations already present in `storage`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndexKind {
    BTree,
    Hash,
    Bitmap,
    VectorHnsw,
    VectorInverted,
    GraphAdjacency,
    FullText,
    DocumentPathValue,
    HybridSearch,
}

impl IndexKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BTree => "btree",
            Self::Hash => "hash",
            Self::Bitmap => "bitmap",
            Self::VectorHnsw => "vector.hnsw",
            Self::VectorInverted => "vector.inverted",
            Self::GraphAdjacency => "graph.adjacency",
            Self::FullText => "text.fulltext",
            Self::DocumentPathValue => "document.pathvalue",
            Self::HybridSearch => "search.hybrid",
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndexConfig {
    pub name: String,
    pub kind: IndexKind,
    pub enabled: bool,
    pub warmup: bool,
    pub updated_at_ms: u128,
}

impl IndexConfig {
    pub fn new(name: impl Into<String>, kind: IndexKind) -> Self {
        Self {
            name: name.into(),
            kind,
            enabled: true,
            warmup: false,
            updated_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn with_warmup(mut self, warmup: bool) -> Self {
        self.warmup = warmup;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub memory_bytes: u64,
    pub entries: usize,
    pub queries: u64,
    pub builds: u64,
    pub errors: u64,
}

#[derive(Debug, Clone)]
pub struct IndexMetric {
    pub name: String,
    pub kind: IndexKind,
    pub enabled: bool,
    pub last_refresh_ms: Option<u128>,
    pub stats: IndexStats,
}

#[derive(Debug, Default)]
pub struct IndexCatalog {
    metrics: BTreeMap<String, IndexMetric>,
}

impl IndexCatalog {
    pub fn register(&mut self, cfg: IndexConfig) {
        let metric = IndexMetric {
            name: cfg.name.clone(),
            kind: cfg.kind,
            enabled: cfg.enabled,
            last_refresh_ms: Some(cfg.updated_at_ms),
            stats: IndexStats::default(),
        };
        self.metrics.insert(cfg.name, metric);
    }

    pub fn snapshot(&self) -> Vec<IndexMetric> {
        self.metrics.values().cloned().collect()
    }

    pub fn enabled(&self) -> Vec<String> {
        self.metrics
            .iter()
            .filter_map(|(name, metric)| metric.enabled.then_some(name.clone()))
            .collect()
    }

    pub fn disable(&mut self, name: &str) -> bool {
        if let Some(metric) = self.metrics.get_mut(name) {
            metric.enabled = false;
        }

        self.metrics.contains_key(name)
    }

    pub fn touch(&mut self, name: &str) {
        if let Some(metric) = self.metrics.get_mut(name) {
            metric.last_refresh_ms = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            );
        }
    }

    pub fn register_default_vector_graph(table: bool, graph: bool) -> Self {
        let mut catalog = Self::default();
        if table {
            catalog.register(IndexConfig::new("metadata-btree", IndexKind::BTree));
        }
        if graph {
            catalog.register(IndexConfig::new(
                "graph-adjacency",
                IndexKind::GraphAdjacency,
            ));
        }
        catalog.register(IndexConfig::new("vector-hnsw", IndexKind::VectorHnsw).with_warmup(true));
        catalog.register(IndexConfig::new(
            "vector-inverted",
            IndexKind::VectorInverted,
        ));
        catalog
    }
}

pub trait IndexRuntime {
    fn describe(&self) -> Vec<IndexMetric>;
    fn apply_metric(&mut self, metric: IndexMetric);
}

impl IndexRuntime for IndexCatalog {
    fn describe(&self) -> Vec<IndexMetric> {
        self.snapshot()
    }

    fn apply_metric(&mut self, metric: IndexMetric) {
        self.metrics.insert(metric.name.clone(), metric);
    }
}

pub type IndexCatalogSnapshot = Vec<IndexMetric>;
