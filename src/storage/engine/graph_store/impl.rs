use std::sync::Arc;

use crate::storage::index::{IndexRegistry, IndexScope};

use super::*;

impl GraphStore {
    /// Create a new empty graph store
    pub fn new() -> Self {
        // Use 16 shards for good parallelism on modern CPUs
        const SHARD_COUNT: usize = 16;

        // Create initial pages
        let initial_node_page = Page::new(PageType::GraphNode, 0);
        let initial_edge_page = Page::new(PageType::GraphEdge, 0);

        Self {
            node_index: ShardedIndex::new(SHARD_COUNT),
            edge_index: EdgeIndex::new(SHARD_COUNT),
            node_secondary: Arc::new(NodeSecondaryIndex::new(8192)),
            node_pages: RwLock::new(vec![initial_node_page]),
            edge_pages: RwLock::new(vec![initial_edge_page]),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(0),
            edge_count: AtomicU64::new(0),
        }
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

    /// Add a node to the graph
    pub fn add_node(
        &self,
        id: &str,
        label: &str,
        node_type: GraphNodeType,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Check if node already exists
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }

        let node = StoredNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type,
            flags: 0,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: None,
            vector_ref: None,
        };

        let encoded = node.encode();

        // Get write lock on node pages
        let mut pages = self
            .node_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_node_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        let location = match page.insert_cell(id.as_bytes(), &encoded) {
            Ok(slot) => RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            },
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphNode, new_page_id);

                let slot = new_page
                    .insert_cell(id.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_node_page.store(new_page_id, Ordering::Release);

                RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                }
            }
        };

        self.node_index.insert(id.to_string(), location);
        self.node_secondary.insert(id, node_type, label);
        self.node_count.fetch_add(1, Ordering::Relaxed);
        Ok(location)
    }

    /// Add a node linked to a table row (for unified queries)
    pub fn add_node_linked(
        &self,
        id: &str,
        label: &str,
        node_type: GraphNodeType,
        table_id: u16,
        row_id: u64,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Check if node already exists
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }

        let node = StoredNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type,
            flags: NODE_FLAG_HAS_TABLE_REF,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: Some(TableRef::new(table_id, row_id)),
            vector_ref: None,
        };

        let encoded = node.encode();

        // Get write lock on node pages
        let mut pages = self
            .node_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_node_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        let location = match page.insert_cell(id.as_bytes(), &encoded) {
            Ok(slot) => RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            },
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphNode, new_page_id);

                let slot = new_page
                    .insert_cell(id.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_node_page.store(new_page_id, Ordering::Release);

                RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                }
            }
        };

        self.node_index.insert(id.to_string(), location);
        self.node_secondary.insert(id, node_type, label);
        self.node_count.fetch_add(1, Ordering::Relaxed);
        Ok(location)
    }

    /// Get table reference for a node (if linked)
    pub fn get_node_table_ref(&self, node_id: &str) -> Option<TableRef> {
        self.get_node(node_id).and_then(|n| n.table_ref)
    }

    /// Add an edge to the graph
    pub fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: GraphEdgeType,
        weight: f32,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Verify nodes exist
        if !self.node_index.contains(source_id) {
            return Err(GraphStoreError::NodeNotFound(source_id.to_string()));
        }
        if !self.node_index.contains(target_id) {
            return Err(GraphStoreError::NodeNotFound(target_id.to_string()));
        }

        let edge = StoredEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            edge_type,
            weight,
            page_id: 0,
            slot: 0,
        };

        let encoded = edge.encode();

        // Create composite key for edge storage
        let edge_key = format!("{}|{}|{}", source_id, edge_type as u8, target_id);

        // Get write lock on edge pages
        let mut pages = self
            .edge_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_edge_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        let location = match page.insert_cell(edge_key.as_bytes(), &encoded) {
            Ok(slot) => RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            },
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphEdge, new_page_id);

                let slot = new_page
                    .insert_cell(edge_key.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_edge_page.store(new_page_id, Ordering::Release);

                RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                }
            }
        };

        // Update edge index (this is the fast path for traversal)
        self.edge_index
            .add_edge(source_id, target_id, edge_type, weight);
        self.edge_count.fetch_add(1, Ordering::Relaxed);

        Ok(location)
    }

    /// Get a node by ID (lock-free read)
    pub fn get_node(&self, id: &str) -> Option<StoredNode> {
        let location = self.node_index.get(id)?;

        let pages = self.node_pages.read().ok()?;
        let page = pages.get(location.page_id as usize)?;

        let (_, value) = page.read_cell(location.slot as usize).ok()?;
        StoredNode::decode(&value, location.page_id, location.slot)
    }

    /// Get all outgoing edges from a node (lock-free read)
    #[inline]
    pub fn outgoing_edges(&self, source_id: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.edge_index.outgoing(source_id)
    }

    /// Get all incoming edges to a node (lock-free read)
    #[inline]
    pub fn incoming_edges(&self, target_id: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.edge_index.incoming(target_id)
    }

    /// Get outgoing edges of a specific type
    #[inline]
    pub fn outgoing_of_type(
        &self,
        source_id: &str,
        edge_type: GraphEdgeType,
    ) -> Vec<(String, f32)> {
        self.edge_index.outgoing_of_type(source_id, edge_type)
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
            // Get outgoing edges for this node
            for (edge_type, target_id, weight) in self.outgoing_edges(&node.id) {
                edges.push(StoredEdge {
                    source_id: node.id.clone(),
                    target_id,
                    edge_type,
                    weight,
                    page_id: 0, // Not needed for iteration
                    slot: 0,    // Not needed for iteration
                });
            }
        }

        edges
    }

    /// Get nodes of a specific type.
    ///
    /// Uses the secondary index to avoid a full page scan: O(k) where k is
    /// the bucket size, plus one node fetch per id.
    pub fn nodes_of_type(&self, node_type: GraphNodeType) -> Vec<StoredNode> {
        self.node_secondary
            .nodes_by_type(node_type)
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

        // Count nodes by type
        for node in self.iter_nodes() {
            stats.nodes_by_type[node.node_type as usize] += 1;
        }

        stats
    }

    /// Serialize to bytes for persistence
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Header: magic(4) + version(4) + node_count(8) + edge_count(8) + node_pages(4) + edge_pages(4)
        buf.extend_from_slice(b"RBGR"); // RedDB GRaph
        buf.extend_from_slice(&1u32.to_le_bytes()); // version
        buf.extend_from_slice(&self.node_count.load(Ordering::Relaxed).to_le_bytes());
        buf.extend_from_slice(&self.edge_count.load(Ordering::Relaxed).to_le_bytes());

        // Serialize node pages
        if let Ok(pages) = self.node_pages.read() {
            buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
            for page in pages.iter() {
                buf.extend_from_slice(page.as_bytes());
            }
        }

        // Serialize edge pages
        if let Ok(pages) = self.edge_pages.read() {
            buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
            for page in pages.iter() {
                buf.extend_from_slice(page.as_bytes());
            }
        }

        buf
    }

    /// Deserialize from bytes
    pub fn deserialize(data: &[u8]) -> Result<Self, GraphStoreError> {
        if data.len() < 32 {
            return Err(GraphStoreError::InvalidData("Too short".to_string()));
        }

        // Verify magic
        if &data[0..4] != b"RBGR" {
            return Err(GraphStoreError::InvalidData("Invalid magic".to_string()));
        }

        let _version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let node_count = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        let edge_count = u64::from_le_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]);

        let mut offset = 24;

        // Read node page count
        let node_page_count = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        // Read node pages
        let mut node_pages = Vec::with_capacity(node_page_count);
        for _ in 0..node_page_count {
            if offset + PAGE_SIZE > data.len() {
                return Err(GraphStoreError::InvalidData(
                    "Truncated node pages".to_string(),
                ));
            }
            let page = Page::from_slice(&data[offset..offset + PAGE_SIZE])
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            node_pages.push(page);
            offset += PAGE_SIZE;
        }

        // Read edge page count
        let edge_page_count = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        // Read edge pages
        let mut edge_pages = Vec::with_capacity(edge_page_count);
        for _ in 0..edge_page_count {
            if offset + PAGE_SIZE > data.len() {
                return Err(GraphStoreError::InvalidData(
                    "Truncated edge pages".to_string(),
                ));
            }
            let page = Page::from_slice(&data[offset..offset + PAGE_SIZE])
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            edge_pages.push(page);
            offset += PAGE_SIZE;
        }

        // Rebuild indexes from pages
        let store = Self {
            node_index: ShardedIndex::new(16),
            edge_index: EdgeIndex::new(16),
            node_secondary: Arc::new(NodeSecondaryIndex::new(8192)),
            node_pages: RwLock::new(node_pages),
            edge_pages: RwLock::new(edge_pages),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(node_count),
            edge_count: AtomicU64::new(edge_count),
        };

        store.rebuild_indexes()?;

        Ok(store)
    }

    /// Rebuild indexes from pages (used after deserialization)
    fn rebuild_indexes(&self) -> Result<(), GraphStoreError> {
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
                        if let Some(node) =
                            StoredNode::decode(&value, page_idx as u32, cell_idx as u16)
                        {
                            self.node_secondary.insert(&id, node.node_type, &node.label);
                        }
                    }
                }
            }

            // Update current node page
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
                        if let Some(edge) =
                            StoredEdge::decode(&value, page_idx as u32, cell_idx as u16)
                        {
                            self.edge_index.add_edge(
                                &edge.source_id,
                                &edge.target_id,
                                edge.edge_type,
                                edge.weight,
                            );
                        }
                    }
                }
            }

            // Update current edge page
            if !pages.is_empty() {
                self.current_edge_page
                    .store((pages.len() - 1) as u32, Ordering::Release);
            }
        }

        Ok(())
    }
}
