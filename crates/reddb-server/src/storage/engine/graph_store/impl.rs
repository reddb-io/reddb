use std::sync::Arc;

use crate::storage::index::{IndexRegistry, IndexScope};

use super::label_registry::Namespace;
use super::*;

impl GraphStore {
    /// Create a new empty graph store with a fresh [`LabelRegistry`] that
    /// has the legacy reserved label IDs (1..=19) pre-seeded so v1
    /// on-disk graph records still decode round-trip.
    pub fn new() -> Self {
        Self::with_registry(Arc::new(LabelRegistry::with_legacy_seed()))
    }

    /// Create an empty graph store sharing the given [`LabelRegistry`]. Use
    /// this when multiple [`GraphStore`] instances should agree on
    /// [`LabelId`] assignments (e.g. when the same database holds several
    /// named graphs).
    pub fn with_registry(registry: Arc<LabelRegistry>) -> Self {
        // Use 16 shards for good parallelism on modern CPUs
        const SHARD_COUNT: usize = 16;

        let initial_node_page = Page::new(PageType::GraphNode, 0);
        let initial_edge_page = Page::new(PageType::GraphEdge, 0);

        Self {
            node_index: ShardedIndex::new(SHARD_COUNT),
            edge_index: EdgeIndex::new(SHARD_COUNT),
            node_secondary: Arc::new(NodeSecondaryIndex::new(8192)),
            registry,
            node_pages: RwLock::new(vec![initial_node_page]),
            edge_pages: RwLock::new(vec![initial_edge_page]),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(0),
            edge_count: AtomicU64::new(0),
        }
    }

    /// Intern an arbitrary node label string. Convenience wrapper over
    /// [`LabelRegistry::intern`]; the returned [`LabelId`] can be passed to
    /// the upcoming label-id-aware insert APIs (PR3).
    pub fn intern_node_label(&self, label: &str) -> Result<LabelId, GraphStoreError> {
        self.registry
            .intern(Namespace::Node, label)
            .map_err(|e| GraphStoreError::InvalidData(e.to_string()))
    }

    /// Intern an arbitrary edge label string.
    pub fn intern_edge_label(&self, label: &str) -> Result<LabelId, GraphStoreError> {
        self.registry
            .intern(Namespace::Edge, label)
            .map_err(|e| GraphStoreError::InvalidData(e.to_string()))
    }

    /// Publish this graph's secondary index into an external
    /// [`IndexRegistry`]. The registry holds an `Arc` pointing to the same
    /// live index, so planners consulting the registry see current stats
    /// without any copy/refresh logic.
    ///
    /// Scope: `IndexScope::graph(collection)`. Idempotent — subsequent
    /// calls replace the previous entry.
    pub fn publish_indexes(&self, registry: &IndexRegistry, collection: &str) {
        registry.register(
            IndexScope::graph(collection),
            Arc::clone(&self.node_secondary) as Arc<dyn crate::storage::index::IndexBase>,
        );
    }

    /// Add a node using a category label string. Interns `category` into the
    /// [`LabelRegistry`] and writes the node in v2 format.
    pub fn add_node_with_label(
        &self,
        id: &str,
        display_label: &str,
        category: &str,
    ) -> Result<RecordLocation, GraphStoreError> {
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }
        let label_id = self.intern_node_label(category)?;
        let node = StoredNode {
            id: id.to_string(),
            label: display_label.to_string(),
            node_type: category.to_string(),
            label_id,
            flags: 0,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: None,
            vector_ref: None,
        };
        let location = self.write_node_record(id, &node)?;
        self.node_index.insert(id.to_string(), location);
        self.node_secondary.insert(id, label_id, display_label);
        self.node_count.fetch_add(1, Ordering::Relaxed);
        Ok(location)
    }

    /// Add an edge using a category label string.
    pub fn add_edge_with_label(
        &self,
        source_id: &str,
        target_id: &str,
        category: &str,
        weight: f32,
    ) -> Result<RecordLocation, GraphStoreError> {
        if !self.node_index.contains(source_id) {
            return Err(GraphStoreError::NodeNotFound(source_id.to_string()));
        }
        if !self.node_index.contains(target_id) {
            return Err(GraphStoreError::NodeNotFound(target_id.to_string()));
        }
        let label_id = self.intern_edge_label(category)?;
        let edge = StoredEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            edge_type: category.to_string(),
            label_id,
            weight,
            page_id: 0,
            slot: 0,
        };
        let location = self.write_edge_record(source_id, target_id, label_id, &edge)?;
        self.edge_index
            .add_edge(source_id, target_id, category, weight);
        self.edge_count.fetch_add(1, Ordering::Relaxed);
        Ok(location)
    }

    /// Internal: encode a [`StoredNode`] and append to the current node page,
    /// rolling over to a new page when full.
    fn write_node_record(
        &self,
        id: &str,
        node: &StoredNode,
    ) -> Result<RecordLocation, GraphStoreError> {
        let encoded = node.encode();
        let mut pages = self
            .node_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;
        let current_page_id = self.current_node_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];
        match page.insert_cell(id.as_bytes(), &encoded) {
            Ok(slot) => Ok(RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            }),
            Err(_) => {
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphNode, new_page_id);
                let slot = new_page
                    .insert_cell(id.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;
                pages.push(new_page);
                self.current_node_page.store(new_page_id, Ordering::Release);
                Ok(RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                })
            }
        }
    }

    /// Internal: encode a [`StoredEdge`] and append it.
    fn write_edge_record(
        &self,
        source_id: &str,
        target_id: &str,
        label_id: LabelId,
        edge: &StoredEdge,
    ) -> Result<RecordLocation, GraphStoreError> {
        let encoded = edge.encode();
        let edge_key = format!("{}|{}|{}", source_id, label_id.as_u32(), target_id);
        let mut pages = self
            .edge_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;
        let current_page_id = self.current_edge_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];
        match page.insert_cell(edge_key.as_bytes(), &encoded) {
            Ok(slot) => Ok(RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            }),
            Err(_) => {
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphEdge, new_page_id);
                let slot = new_page
                    .insert_cell(edge_key.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;
                pages.push(new_page);
                self.current_edge_page.store(new_page_id, Ordering::Release);
                Ok(RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                })
            }
        }
    }

    /// Add a node linked to a table row (for unified queries).
    pub fn add_node_linked(
        &self,
        id: &str,
        label: &str,
        category: &str,
        table_id: u16,
        row_id: u64,
    ) -> Result<RecordLocation, GraphStoreError> {
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }
        let label_id = self.intern_node_label(category)?;
        let node = StoredNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type: category.to_string(),
            label_id,
            flags: NODE_FLAG_HAS_TABLE_REF,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: Some(TableRef::new(table_id, row_id)),
            vector_ref: None,
        };

        let location = self.write_node_record(id, &node)?;
        self.node_index.insert(id.to_string(), location);
        self.node_secondary.insert(id, label_id, label);
        self.node_count.fetch_add(1, Ordering::Relaxed);
        Ok(location)
    }

    /// Get table reference for a node (if linked)
    pub fn get_node_table_ref(&self, node_id: &str) -> Option<TableRef> {
        self.get_node(node_id).and_then(|n| n.table_ref)
    }

    /// Get a node by ID (lock-free read)
    pub fn get_node(&self, id: &str) -> Option<StoredNode> {
        let location = self.node_index.get(id)?;

        let pages = self.node_pages.read().ok()?;
        let page = pages.get(location.page_id as usize)?;

        let (_, value) = page.read_cell(location.slot as usize).ok()?;
        StoredNode::decode(&value, location.page_id, location.slot)
    }

    /// Get all outgoing edges from a node `(edge_label, target, weight)`.
    #[inline]
    pub fn outgoing_edges(&self, source_id: &str) -> Vec<(String, String, f32)> {
        self.edge_index.outgoing(source_id)
    }

    /// Get all incoming edges to a node `(edge_label, source, weight)`.
    #[inline]
    pub fn incoming_edges(&self, target_id: &str) -> Vec<(String, String, f32)> {
        self.edge_index.incoming(target_id)
    }

    /// Get outgoing edges of a specific label.
    #[inline]
    pub fn outgoing_of_type(&self, source_id: &str, edge_label: &str) -> Vec<(String, f32)> {
        self.edge_index.outgoing_of_type(source_id, edge_label)
    }

    /// Check if a node exists
    #[inline]
    pub fn has_node(&self, id: &str) -> bool {
        self.node_index.contains(id)
    }

    /// Get node count
    #[inline]
    pub fn node_count(&self) -> u64 {
        self.node_count.load(Ordering::Relaxed)
    }

    /// Get edge count
    #[inline]
    pub fn edge_count(&self) -> u64 {
        self.edge_count.load(Ordering::Relaxed)
    }

    /// Iterate over all nodes (streaming)
    pub fn iter_nodes(&self) -> NodeIterator<'_> {
        NodeIterator {
            store: self,
            page_idx: 0,
            cell_idx: 0,
        }
    }

    /// Iterate all edges in the graph
    ///
    /// This collects outgoing edges from all nodes to build a complete edge list.
    /// Returns StoredEdge structs with source, target, type, and weight.
    pub fn iter_all_edges(&self) -> Vec<StoredEdge> {
        let mut edges = Vec::new();

        for node in self.iter_nodes() {
            for (edge_label, target_id, weight) in self.outgoing_edges(&node.id) {
                let label_id = self
                    .registry
                    .lookup(Namespace::Edge, &edge_label)
                    .unwrap_or(UNSET_LABEL_ID);
                edges.push(StoredEdge {
                    source_id: node.id.clone(),
                    target_id,
                    edge_type: edge_label,
                    label_id,
                    weight,
                    page_id: 0,
                    slot: 0,
                });
            }
        }

        edges
    }

    /// Get nodes whose category resolves to `label_id`. O(k) via secondary
    /// index plus one fetch per id.
    pub fn nodes_of_label(&self, label_id: LabelId) -> Vec<StoredNode> {
        self.node_secondary
            .nodes_by_type(label_id)
            .into_iter()
            .filter_map(|id| self.get_node(&id))
            .collect()
    }

    /// Get nodes with a given label. Backed by the secondary inverted index
    /// (`label → node_id set`) with a bloom-filter pre-check for absent
    /// labels.
    pub fn nodes_by_label(&self, label: &str) -> Vec<StoredNode> {
        self.node_secondary
            .nodes_by_label(label)
            .into_iter()
            .filter_map(|id| self.get_node(&id))
            .collect()
    }

    /// Get nodes whose category label (as registered in the
    /// [`LabelRegistry`]) matches the given string. Replaces the
    /// enum-typed [`nodes_of_type`] for callers that work with arbitrary
    /// user-defined labels.
    ///
    /// O(k) lookup via secondary index keyed by [`LabelId`].
    pub fn nodes_with_category(&self, category: &str) -> Vec<StoredNode> {
        let Some(label_id) = self.registry.lookup(Namespace::Node, category) else {
            return Vec::new();
        };
        self.nodes_of_label(label_id)
    }

    /// Returns `true` iff the label is *possibly* present. Bloom-backed
    /// fast path for planners that want to skip a traversal without paying
    /// the set lookup cost.
    pub fn may_contain_label(&self, label: &str) -> bool {
        self.node_secondary.may_contain_label(label)
    }

    /// Read-only handle to the secondary index (for planner/diagnostics).
    pub fn node_secondary_index(&self) -> &NodeSecondaryIndex {
        &self.node_secondary
    }

    /// Get statistics
    pub fn stats(&self) -> GraphStats {
        let mut stats = GraphStats {
            node_count: self.node_count.load(Ordering::Relaxed),
            edge_count: self.edge_count.load(Ordering::Relaxed),
            node_pages: self.node_pages.read().map(|p| p.len() as u32).unwrap_or(0),
            edge_pages: self.edge_pages.read().map(|p| p.len() as u32).unwrap_or(0),
            ..Default::default()
        };

        // Per-category counts derived from the secondary index — O(number
        // of distinct labels) instead of O(node_count). The secondary
        // index already maintains the bucket cardinalities incrementally
        // on every add/remove, so this is essentially free.
        for (label_id, count) in self.node_secondary.label_id_counts() {
            if let Some((Namespace::Node, label)) = self.registry.resolve(label_id) {
                stats.nodes_by_label.insert(label, count);
            }
        }

        stats
    }

    /// Serialize to bytes for persistence (file format v2: includes the
    /// embedded [`LabelRegistry`] catalog right after the fixed header).
    pub fn serialize(&self) -> Vec<u8> {
        let registry_bytes = self.registry.encode().unwrap_or_default();
        let node_pages = self
            .node_pages
            .read()
            .map(|pages| pages.iter().map(|page| page.as_bytes().to_vec()).collect())
            .unwrap_or_default();
        let edge_pages = self
            .edge_pages
            .read()
            .map(|pages| pages.iter().map(|page| page.as_bytes().to_vec()).collect())
            .unwrap_or_default();

        reddb_file::encode_graph_store_frame(&reddb_file::GraphStoreFrame {
            version: reddb_file::GRAPH_STORE_VERSION_V2,
            node_count: self.node_count.load(Ordering::Relaxed),
            edge_count: self.edge_count.load(Ordering::Relaxed),
            registry_bytes: Some(registry_bytes),
            node_pages,
            edge_pages,
        })
        .expect("in-memory graph store should encode")
    }

    /// Deserialize from bytes. Dual-path: a v1 file (no embedded registry,
    /// 1-byte enum discriminants) is read with [`StoredNode::decode_v1`]
    /// against a freshly-seeded legacy registry. A v2 file restores the
    /// registry from its embedded blob and decodes records via
    /// [`StoredNode::decode`].
    pub fn deserialize(data: &[u8]) -> Result<Self, GraphStoreError> {
        let frame = reddb_file::decode_graph_store_frame(data, PAGE_SIZE)
            .map_err(|e| GraphStoreError::InvalidData(e.to_string()))?;

        // V2 carries the registry blob inline. V1 has none (legacy seed).
        let registry: Arc<LabelRegistry> = match frame.version {
            1 => Arc::new(LabelRegistry::with_legacy_seed()),
            2 => {
                let registry_bytes = frame.registry_bytes.as_deref().ok_or_else(|| {
                    GraphStoreError::InvalidData("Missing registry blob".to_string())
                })?;
                let reg = LabelRegistry::decode(registry_bytes)
                    .map_err(|e| GraphStoreError::InvalidData(e.to_string()))?;
                Arc::new(reg)
            }
            v => {
                return Err(GraphStoreError::InvalidData(format!(
                    "Unsupported graph file version {}",
                    v
                )));
            }
        };

        let mut node_pages = Vec::with_capacity(frame.node_pages.len());
        for page_bytes in &frame.node_pages {
            let page = Page::from_slice(page_bytes)
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            node_pages.push(page);
        }

        let mut edge_pages = Vec::with_capacity(frame.edge_pages.len());
        for page_bytes in &frame.edge_pages {
            let page = Page::from_slice(page_bytes)
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            edge_pages.push(page);
        }

        // V1 records on disk use the legacy 1-byte enum header, which the
        // rest of GraphStore (get_node, iterators) does not understand. Migrate
        // in place: decode every v1 cell, re-insert via the v2 write path.
        if frame.version == 1 {
            let store = Self::with_registry(Arc::clone(&registry));
            for (page_idx, page) in node_pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((_, value)) = page.read_cell(cell_idx) {
                        if let Some(n) =
                            StoredNode::decode_v1(&value, page_idx as u32, cell_idx as u16)
                        {
                            // V1 node_type already carries the canonical
                            // legacy label string thanks to the v1 decoder.
                            store.add_node_with_label(&n.id, &n.label, &n.node_type)?;
                        }
                    }
                }
            }
            for (page_idx, page) in edge_pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((_, value)) = page.read_cell(cell_idx) {
                        if let Some(e) =
                            StoredEdge::decode_v1(&value, page_idx as u32, cell_idx as u16)
                        {
                            // Skip edges whose endpoints failed to migrate.
                            if !store.has_node(&e.source_id) || !store.has_node(&e.target_id) {
                                continue;
                            }
                            store.add_edge_with_label(
                                &e.source_id,
                                &e.target_id,
                                &e.edge_type,
                                e.weight,
                            )?;
                        }
                    }
                }
            }
            // Sanity-check counts (v1 file headers can theoretically lie; a
            // mismatch here points at a corrupt blob, but is not fatal —
            // the store reflects what we successfully migrated).
            let _ = (frame.node_count, frame.edge_count);
            return Ok(store);
        }

        let store = Self {
            node_index: ShardedIndex::new(16),
            edge_index: EdgeIndex::new(16),
            node_secondary: Arc::new(NodeSecondaryIndex::new(8192)),
            registry,
            node_pages: RwLock::new(node_pages),
            edge_pages: RwLock::new(edge_pages),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(frame.node_count),
            edge_count: AtomicU64::new(frame.edge_count),
        };

        store.rebuild_indexes(frame.version)?;

        Ok(store)
    }

    /// Rebuild indexes from pages. `version` selects the on-disk record
    /// format used when each cell was written.
    fn rebuild_indexes(&self, version: u32) -> Result<(), GraphStoreError> {
        let decode_node = |bytes: &[u8], page_idx: u32, slot: u16| match version {
            1 => StoredNode::decode_v1(bytes, page_idx, slot),
            _ => StoredNode::decode(bytes, page_idx, slot),
        };
        let decode_edge = |bytes: &[u8], page_idx: u32, slot: u16| match version {
            1 => StoredEdge::decode_v1(bytes, page_idx, slot),
            _ => StoredEdge::decode(bytes, page_idx, slot),
        };

        // Rebuild node + secondary index
        self.node_secondary.clear();
        if let Ok(pages) = self.node_pages.read() {
            for (page_idx, page) in pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((key, value)) = page.read_cell(cell_idx) {
                        let id = String::from_utf8_lossy(&key).to_string();
                        self.node_index.insert(
                            id.clone(),
                            RecordLocation {
                                page_id: page_idx as u32,
                                slot: cell_idx as u16,
                            },
                        );
                        if let Some(node) = decode_node(&value, page_idx as u32, cell_idx as u16) {
                            self.node_secondary.insert(&id, node.label_id, &node.label);
                        }
                    }
                }
            }

            if !pages.is_empty() {
                self.current_node_page
                    .store((pages.len() - 1) as u32, Ordering::Release);
            }
        }

        // Rebuild edge index
        if let Ok(pages) = self.edge_pages.read() {
            for (page_idx, page) in pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((_, value)) = page.read_cell(cell_idx) {
                        if let Some(edge) = decode_edge(&value, page_idx as u32, cell_idx as u16) {
                            self.edge_index.add_edge(
                                &edge.source_id,
                                &edge.target_id,
                                &edge.edge_type,
                                edge.weight,
                            );
                        }
                    }
                }
            }

            if !pages.is_empty() {
                self.current_edge_page
                    .store((pages.len() - 1) as u32, Ordering::Release);
            }
        }

        Ok(())
    }
}
