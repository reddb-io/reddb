use super::*;

impl RedDB {
    pub(crate) fn native_registry_summary_from_metadata(
        &self,
        metadata: &PhysicalMetadataFile,
    ) -> NativeRegistrySummary {
        const SAMPLE_LIMIT: usize = 16;

        let collection_names: Vec<_> = metadata
            .catalog
            .stats_by_collection
            .keys()
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect();
        let indexes: Vec<_> = metadata
            .indexes
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|index| NativeRegistryIndexSummary {
                name: index.name.clone(),
                kind: index.kind.as_str().to_string(),
                collection: index.collection.clone(),
                enabled: index.enabled,
                entries: index.entries as u64,
                estimated_memory_bytes: index.estimated_memory_bytes,
                last_refresh_ms: index.last_refresh_ms,
                backend: index.backend.clone(),
            })
            .collect();
        let graph_projections: Vec<_> = metadata
            .graph_projections
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|projection| NativeRegistryProjectionSummary {
                name: projection.name.clone(),
                source: projection.source.clone(),
                created_at_unix_ms: projection.created_at_unix_ms,
                updated_at_unix_ms: projection.updated_at_unix_ms,
                node_labels: projection.node_labels.clone(),
                node_types: projection.node_types.clone(),
                edge_labels: projection.edge_labels.clone(),
                last_materialized_sequence: projection.last_materialized_sequence,
            })
            .collect();
        let analytics_jobs = metadata
            .analytics_jobs
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|job| NativeRegistryJobSummary {
                id: job.id.clone(),
                kind: job.kind.clone(),
                projection: job.projection.clone(),
                state: job.state.clone(),
                created_at_unix_ms: job.created_at_unix_ms,
                updated_at_unix_ms: job.updated_at_unix_ms,
                last_run_sequence: job.last_run_sequence,
                metadata: job.metadata.clone(),
            })
            .collect::<Vec<_>>();
        let vector_artifacts = self
            .native_vector_artifact_records()
            .into_iter()
            .map(|(summary, _)| summary)
            .take(SAMPLE_LIMIT)
            .collect::<Vec<_>>();
        let vector_artifact_count = self.native_vector_artifact_collection_count() as u32;

        NativeRegistrySummary {
            collection_count: metadata.catalog.total_collections as u32,
            index_count: metadata.indexes.len() as u32,
            graph_projection_count: metadata.graph_projections.len() as u32,
            analytics_job_count: metadata.analytics_jobs.len() as u32,
            vector_artifact_count,
            collections_complete: metadata.catalog.stats_by_collection.len() <= SAMPLE_LIMIT,
            indexes_complete: metadata.indexes.len() <= SAMPLE_LIMIT,
            graph_projections_complete: metadata.graph_projections.len() <= SAMPLE_LIMIT,
            analytics_jobs_complete: metadata.analytics_jobs.len() <= SAMPLE_LIMIT,
            vector_artifacts_complete: vector_artifact_count as usize <= SAMPLE_LIMIT,
            omitted_collection_count: metadata
                .catalog
                .stats_by_collection
                .len()
                .saturating_sub(collection_names.len())
                as u32,
            omitted_index_count: metadata.indexes.len().saturating_sub(indexes.len()) as u32,
            omitted_graph_projection_count: metadata
                .graph_projections
                .len()
                .saturating_sub(graph_projections.len())
                as u32,
            omitted_analytics_job_count: metadata
                .analytics_jobs
                .len()
                .saturating_sub(analytics_jobs.len())
                as u32,
            omitted_vector_artifact_count: vector_artifact_count
                .saturating_sub(vector_artifacts.len() as u32),
            collection_names,
            indexes,
            graph_projections,
            analytics_jobs,
            vector_artifacts,
        }
    }

    fn native_vector_artifact_collection_count(&self) -> usize {
        self.native_vector_artifact_records().len()
    }

    pub(crate) fn native_vector_artifact_records(
        &self,
    ) -> Vec<(NativeVectorArtifactSummary, Vec<u8>)> {
        let mut artifacts = Vec::new();
        for collection in self.store.list_collections() {
            let Some(manager) = self.store.get_collection(&collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            let mut vectors = Vec::new();
            let mut graph_edges = Vec::new();
            let mut fulltext_documents = Vec::new();
            let mut document_records = Vec::new();
            for entity in entities {
                match entity.data {
                    EntityData::Vector(vector) => {
                        if !vector.dense.is_empty() {
                            vectors.push((entity.id, vector.dense));
                        }
                    }
                    EntityData::Edge(edge) => {
                        if let EntityKind::GraphEdge {
                            label,
                            from_node,
                            to_node,
                            ..
                        } = entity.kind
                        {
                            graph_edges.push((entity.id, from_node, to_node, label, edge.weight));
                        }
                    }
                    data => {
                        let text = Self::native_fulltext_text_for_entity(&data);
                        if !text.trim().is_empty() {
                            fulltext_documents.push((entity.id, text));
                        }
                        if let Some(document) =
                            Self::native_document_pathvalue_for_entity(entity.id, &data)
                        {
                            document_records.push(document);
                        }
                    }
                }
            }
            if !vectors.is_empty() {
                let dimension = vectors[0].1.len();
                let mut hnsw = HnswIndex::with_dimension(dimension);
                for (id, vector) in vectors
                    .into_iter()
                    .filter(|(_, vector)| vector.len() == dimension)
                {
                    hnsw.insert_with_id(id.raw(), vector);
                }
                let stats = hnsw.stats();
                let bytes = hnsw.to_bytes();
                let summary = NativeVectorArtifactSummary {
                    collection: collection.clone(),
                    artifact_kind: "hnsw".to_string(),
                    vector_count: stats.node_count as u64,
                    dimension: stats.dimension as u32,
                    max_layer: stats.max_layer as u32,
                    serialized_bytes: bytes.len() as u64,
                    checksum: crate::storage::engine::crc32(&bytes) as u64,
                };
                artifacts.push((summary, bytes));

                let n_lists = ((stats.node_count as f64).sqrt().ceil() as usize).max(1);
                let mut ivf = IvfIndex::new(IvfConfig::new(dimension, n_lists));
                let training = manager
                    .query_all(|_| true)
                    .into_iter()
                    .filter_map(|entity| match entity.data {
                        EntityData::Vector(vector) if vector.dense.len() == dimension => {
                            Some(vector.dense)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                ivf.train(&training);
                let items = manager
                    .query_all(|_| true)
                    .into_iter()
                    .filter_map(|entity| match entity.data {
                        EntityData::Vector(vector) if vector.dense.len() == dimension => {
                            Some((entity.id.raw(), vector.dense))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                ivf.add_batch_with_ids(items);
                let ivf_stats = ivf.stats();
                let ivf_bytes = ivf.to_bytes();
                let ivf_summary = NativeVectorArtifactSummary {
                    collection: collection.clone(),
                    artifact_kind: "ivf".to_string(),
                    vector_count: ivf_stats.total_vectors as u64,
                    dimension: ivf_stats.dimension as u32,
                    max_layer: ivf_stats.n_lists as u32,
                    serialized_bytes: ivf_bytes.len() as u64,
                    checksum: crate::storage::engine::crc32(&ivf_bytes) as u64,
                };
                artifacts.push((ivf_summary, ivf_bytes));
            }

            if !graph_edges.is_empty() {
                let bytes = Self::serialize_native_graph_adjacency_artifact(&graph_edges);
                let (edge_count, node_count, label_count) =
                    Self::inspect_native_graph_adjacency_artifact(&bytes).unwrap_or((0, 0, 0));
                let summary = NativeVectorArtifactSummary {
                    collection: collection.clone(),
                    artifact_kind: "graph.adjacency".to_string(),
                    vector_count: edge_count,
                    dimension: node_count as u32,
                    max_layer: label_count,
                    serialized_bytes: bytes.len() as u64,
                    checksum: crate::storage::engine::crc32(&bytes) as u64,
                };
                artifacts.push((summary, bytes));
            }

            if !fulltext_documents.is_empty() {
                let bytes =
                    Self::serialize_native_fulltext_artifact(&collection, &fulltext_documents);
                let (doc_count, term_count, posting_count) =
                    Self::inspect_native_fulltext_artifact(&bytes).unwrap_or((0, 0, 0));
                let summary = NativeVectorArtifactSummary {
                    collection: collection.clone(),
                    artifact_kind: "text.fulltext".to_string(),
                    vector_count: posting_count,
                    dimension: doc_count as u32,
                    max_layer: term_count as u32,
                    serialized_bytes: bytes.len() as u64,
                    checksum: crate::storage::engine::crc32(&bytes) as u64,
                };
                artifacts.push((summary, bytes));
            }

            if !document_records.is_empty() {
                let bytes = Self::serialize_native_document_pathvalue_artifact(
                    &collection,
                    &document_records,
                );
                let (doc_count, path_count, value_count, unique_value_count) =
                    Self::inspect_native_document_pathvalue_artifact(&bytes)
                        .unwrap_or((0, 0, 0, 0));
                let _ = unique_value_count;
                let summary = NativeVectorArtifactSummary {
                    collection: collection.clone(),
                    artifact_kind: "document.pathvalue".to_string(),
                    vector_count: value_count,
                    dimension: doc_count as u32,
                    max_layer: path_count as u32,
                    serialized_bytes: bytes.len() as u64,
                    checksum: crate::storage::engine::crc32(&bytes) as u64,
                };
                artifacts.push((summary, bytes));
            }
        }
        artifacts
    }

    fn serialize_native_graph_adjacency_artifact(
        edges: &[(EntityId, String, String, String, f32)],
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(32 + edges.len() * 48);
        data.extend_from_slice(b"RDGA");
        data.extend_from_slice(&(edges.len() as u32).to_le_bytes());
        for (edge_id, from_node, to_node, label, weight) in edges {
            data.extend_from_slice(&edge_id.raw().to_le_bytes());
            Self::push_native_artifact_string(&mut data, from_node);
            Self::push_native_artifact_string(&mut data, to_node);
            Self::push_native_artifact_string(&mut data, label);
            data.extend_from_slice(&weight.to_le_bytes());
        }
        data
    }

    pub(crate) fn inspect_native_graph_adjacency_artifact(
        bytes: &[u8],
    ) -> Result<(u64, u64, u32), String> {
        if bytes.len() < 8 || &bytes[0..4] != b"RDGA" {
            return Err("invalid graph adjacency artifact".to_string());
        }
        let mut pos = 4usize;
        let edge_count =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
        pos += 4;
        let mut nodes = BTreeSet::new();
        let mut labels = BTreeSet::new();
        for _ in 0..edge_count {
            if pos + 8 > bytes.len() {
                return Err("truncated graph adjacency artifact".to_string());
            }
            pos += 8;
            let from = Self::read_native_artifact_string(bytes, &mut pos)?;
            let to = Self::read_native_artifact_string(bytes, &mut pos)?;
            let label = Self::read_native_artifact_string(bytes, &mut pos)?;
            if pos + 4 > bytes.len() {
                return Err("truncated graph adjacency artifact weight".to_string());
            }
            pos += 4;
            nodes.insert(from);
            nodes.insert(to);
            labels.insert(label);
        }
        Ok((edge_count as u64, nodes.len() as u64, labels.len() as u32))
    }

    fn serialize_native_fulltext_artifact(
        collection: &str,
        documents: &[(EntityId, String)],
    ) -> Vec<u8> {
        let mut postings: BTreeMap<String, Vec<(u64, u32)>> = BTreeMap::new();
        for (entity_id, text) in documents {
            let mut frequencies: BTreeMap<String, u32> = BTreeMap::new();
            for token in Self::native_fulltext_tokenize(text) {
                *frequencies.entry(token).or_insert(0) += 1;
            }
            for (token, count) in frequencies {
                postings
                    .entry(token)
                    .or_default()
                    .push((entity_id.raw(), count));
            }
        }

        let mut data = Vec::with_capacity(64 + postings.len() * 32);
        data.extend_from_slice(b"RDFT");
        Self::push_native_artifact_string(&mut data, collection);
        data.extend_from_slice(&(documents.len() as u32).to_le_bytes());
        data.extend_from_slice(&(postings.len() as u32).to_le_bytes());
        for (term, entries) in postings {
            Self::push_native_artifact_string(&mut data, &term);
            data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for (entity_id, term_count) in entries {
                data.extend_from_slice(&entity_id.to_le_bytes());
                data.extend_from_slice(&term_count.to_le_bytes());
            }
        }
        data
    }

    pub(crate) fn inspect_native_fulltext_artifact(
        bytes: &[u8],
    ) -> Result<(u64, u64, u64), String> {
        if bytes.len() < 12 || &bytes[0..4] != b"RDFT" {
            return Err("invalid fulltext artifact".to_string());
        }
        let mut pos = 4usize;
        let _collection = Self::read_native_artifact_string(bytes, &mut pos)?;
        if pos + 8 > bytes.len() {
            return Err("truncated fulltext artifact".to_string());
        }
        let doc_count =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as u64;
        pos += 4;
        let term_count =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as u64;
        pos += 4;
        let mut posting_count = 0u64;
        for _ in 0..term_count {
            let _term = Self::read_native_artifact_string(bytes, &mut pos)?;
            if pos + 4 > bytes.len() {
                return Err("truncated fulltext posting count".to_string());
            }
            let entry_count =
                u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as u64;
            pos += 4;
            posting_count += entry_count;
            let bytes_needed = entry_count as usize * 12;
            if pos + bytes_needed > bytes.len() {
                return Err("truncated fulltext postings".to_string());
            }
            pos += bytes_needed;
        }
        Ok((doc_count, term_count, posting_count))
    }

    fn serialize_native_document_pathvalue_artifact(
        collection: &str,
        documents: &[(EntityId, Vec<(String, String)>)],
    ) -> Vec<u8> {
        let total_entries: usize = documents.iter().map(|(_, entries)| entries.len()).sum();
        let mut data = Vec::with_capacity(64 + total_entries * 48);
        data.extend_from_slice(b"RDDP");
        Self::push_native_artifact_string(&mut data, collection);
        data.extend_from_slice(&(documents.len() as u32).to_le_bytes());
        data.extend_from_slice(&(total_entries as u32).to_le_bytes());
        for (entity_id, entries) in documents {
            data.extend_from_slice(&entity_id.raw().to_le_bytes());
            data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for (path, value) in entries {
                Self::push_native_artifact_string(&mut data, path);
                Self::push_native_artifact_string(&mut data, value);
            }
        }
        data
    }

    pub(crate) fn inspect_native_document_pathvalue_artifact(
        bytes: &[u8],
    ) -> Result<(u64, u64, u64, u64), String> {
        if bytes.len() < 12 || &bytes[0..4] != b"RDDP" {
            return Err("invalid document path/value artifact".to_string());
        }
        let mut pos = 4usize;
        let _collection = Self::read_native_artifact_string(bytes, &mut pos)?;
        if pos + 8 > bytes.len() {
            return Err("truncated document path/value artifact".to_string());
        }
        let doc_count =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as u64;
        pos += 4;
        let total_entries =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as u64;
        pos += 4;
        let mut paths = BTreeSet::new();
        let mut values = BTreeSet::new();
        let mut seen_entries = 0u64;
        for _ in 0..doc_count {
            if pos + 12 > bytes.len() {
                return Err("truncated document path/value record".to_string());
            }
            pos += 8;
            let entry_count =
                u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            pos += 4;
            for _ in 0..entry_count {
                let path = Self::read_native_artifact_string(bytes, &mut pos)?;
                let value = Self::read_native_artifact_string(bytes, &mut pos)?;
                paths.insert(path);
                values.insert(value);
                seen_entries += 1;
            }
        }
        if seen_entries != total_entries {
            return Err("document path/value artifact entry count mismatch".to_string());
        }
        Ok((
            doc_count,
            paths.len() as u64,
            total_entries,
            values.len() as u64,
        ))
    }

    fn native_document_pathvalue_for_entity(
        entity_id: EntityId,
        data: &EntityData,
    ) -> Option<(EntityId, Vec<(String, String)>)> {
        let mut entries = Vec::new();
        match data {
            EntityData::Row(row) => {
                if let Some(named) = &row.named {
                    for (key, value) in named {
                        Self::collect_native_document_entries_from_value(key, value, &mut entries);
                    }
                }
                for (idx, value) in row.columns.iter().enumerate() {
                    let path = format!("columns[{idx}]");
                    Self::collect_native_document_entries_from_value(&path, value, &mut entries);
                }
            }
            EntityData::Node(node) => {
                for (key, value) in &node.properties {
                    Self::collect_native_document_entries_from_value(key, value, &mut entries);
                }
            }
            EntityData::Edge(edge) => {
                for (key, value) in &edge.properties {
                    Self::collect_native_document_entries_from_value(key, value, &mut entries);
                }
            }
            EntityData::Vector(_) => {}
            EntityData::TimeSeries(_) => {}
            EntityData::QueueMessage(_) => {}
        }
        if entries.is_empty() {
            None
        } else {
            Some((entity_id, entries))
        }
    }

    fn collect_native_document_entries_from_value(
        path: &str,
        value: &Value,
        out: &mut Vec<(String, String)>,
    ) {
        match value {
            Value::Json(bytes) | Value::Blob(bytes) => {
                if let Ok(json) = crate::json::from_slice::<JsonValue>(bytes) {
                    Self::collect_native_document_entries_from_json(path, &json, out);
                }
            }
            _ => {}
        }
    }

    fn collect_native_document_entries_from_json(
        path: &str,
        value: &JsonValue,
        out: &mut Vec<(String, String)>,
    ) {
        match value {
            JsonValue::Object(entries) => {
                for (key, value) in entries {
                    let next = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    Self::collect_native_document_entries_from_json(&next, value, out);
                }
            }
            JsonValue::Array(items) => {
                for (idx, value) in items.iter().enumerate() {
                    let next = format!("{path}[{idx}]");
                    Self::collect_native_document_entries_from_json(&next, value, out);
                }
            }
            _ => {
                if let Some(text) = Self::native_json_scalar_text(value) {
                    out.push((path.to_string(), text));
                }
            }
        }
    }

    fn native_json_scalar_text(value: &JsonValue) -> Option<String> {
        match value {
            JsonValue::Null => None,
            JsonValue::Bool(value) => Some(value.to_string()),
            JsonValue::Number(value) => Some(value.to_string()),
            JsonValue::String(value) => Some(value.clone()),
            JsonValue::Array(_) | JsonValue::Object(_) => None,
        }
    }

    fn native_fulltext_text_for_entity(data: &EntityData) -> String {
        match data {
            EntityData::Row(row) => {
                let mut parts = Vec::new();
                if let Some(named) = &row.named {
                    for value in named.values() {
                        if let Some(text) = Self::native_value_text(value) {
                            parts.push(text);
                        }
                    }
                }
                for value in &row.columns {
                    if let Some(text) = Self::native_value_text(value) {
                        parts.push(text);
                    }
                }
                parts.join(" ")
            }
            EntityData::Node(node) => node
                .properties
                .values()
                .filter_map(Self::native_value_text)
                .collect::<Vec<_>>()
                .join(" "),
            EntityData::Edge(edge) => edge
                .properties
                .values()
                .filter_map(Self::native_value_text)
                .collect::<Vec<_>>()
                .join(" "),
            EntityData::Vector(vector) => vector.content.clone().unwrap_or_default(),
            EntityData::TimeSeries(ts) => ts.metric.clone(),
            EntityData::QueueMessage(_) => String::new(),
        }
    }

    fn native_value_text(value: &Value) -> Option<String> {
        match value {
            Value::Text(value) => Some(value.clone()),
            Value::Json(value) => String::from_utf8(value.clone()).ok(),
            Value::Blob(value) => String::from_utf8(value.clone()).ok(),
            Value::Integer(value) => Some(value.to_string()),
            Value::UnsignedInteger(value) => Some(value.to_string()),
            Value::Float(value) => Some(value.to_string()),
            Value::Boolean(value) => Some(value.to_string()),
            Value::IpAddr(value) => Some(value.to_string()),
            Value::NodeRef(value) => Some(value.clone()),
            Value::EdgeRef(value) => Some(value.clone()),
            Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
            Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
            Value::Timestamp(value) => Some(value.to_string()),
            Value::Duration(value) => Some(value.to_string()),
            Value::Uuid(_) | Value::MacAddr(_) | Value::Vector(_) | Value::Null => None,
            Value::Color([r, g, b]) => Some(format!("#{:02X}{:02X}{:02X}", r, g, b)),
            Value::Email(s) => Some(s.clone()),
            Value::Url(s) => Some(s.clone()),
            Value::Phone(n) => Some(format!("+{}", n)),
            Value::Semver(packed) => Some(format!(
                "{}.{}.{}",
                packed / 1_000_000,
                (packed / 1_000) % 1_000,
                packed % 1_000
            )),
            Value::Cidr(ip, prefix) => Some(format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            )),
            Value::Date(days) => Some(days.to_string()),
            Value::Time(ms) => {
                let total_secs = ms / 1000;
                Some(format!(
                    "{:02}:{:02}:{:02}",
                    total_secs / 3600,
                    (total_secs / 60) % 60,
                    total_secs % 60
                ))
            }
            Value::Decimal(v) => Some(format!("{:.4}", *v as f64 / 10_000.0)),
            Value::EnumValue(i) => Some(format!("enum({})", i)),
            Value::Array(_) => None,
            Value::TimestampMs(ms) => Some(ms.to_string()),
            Value::Ipv4(ip) => Some(format!(
                "{}.{}.{}.{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF
            )),
            Value::Ipv6(bytes) => Some(format!("{}", std::net::Ipv6Addr::from(*bytes))),
            Value::Subnet(ip, mask) => {
                let prefix = mask.leading_ones();
                Some(format!(
                    "{}.{}.{}.{}/{}",
                    (ip >> 24) & 0xFF,
                    (ip >> 16) & 0xFF,
                    (ip >> 8) & 0xFF,
                    ip & 0xFF,
                    prefix
                ))
            }
            Value::Port(p) => Some(p.to_string()),
            Value::Latitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
            Value::Longitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
            Value::GeoPoint(lat, lon) => Some(format!(
                "{:.6},{:.6}",
                *lat as f64 / 1_000_000.0,
                *lon as f64 / 1_000_000.0
            )),
            Value::Country2(c) => Some(String::from_utf8_lossy(c).to_string()),
            Value::Country3(c) => Some(String::from_utf8_lossy(c).to_string()),
            Value::Lang2(c) => Some(String::from_utf8_lossy(c).to_string()),
            Value::Lang5(c) => Some(String::from_utf8_lossy(c).to_string()),
            Value::Currency(c) => Some(String::from_utf8_lossy(c).to_string()),
            Value::ColorAlpha([r, g, b, a]) => {
                Some(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a))
            }
            Value::BigInt(v) => Some(v.to_string()),
            Value::KeyRef(col, key) => Some(format!("{}:{}", col, key)),
            Value::DocRef(col, id) => Some(format!("{}#{}", col, id)),
            Value::TableRef(name) => Some(name.clone()),
            Value::PageRef(page_id) => Some(format!("page:{}", page_id)),
        }
    }

    fn native_fulltext_tokenize(text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() >= 2)
            .map(|s| s.to_string())
            .collect()
    }

    fn push_native_artifact_string(buf: &mut Vec<u8>, value: &str) {
        let bytes = value.as_bytes();
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
    }

    fn read_native_artifact_string(bytes: &[u8], pos: &mut usize) -> Result<String, String> {
        if *pos + 4 > bytes.len() {
            return Err("truncated native artifact string length".to_string());
        }
        let len = u32::from_le_bytes([
            bytes[*pos],
            bytes[*pos + 1],
            bytes[*pos + 2],
            bytes[*pos + 3],
        ]) as usize;
        *pos += 4;
        if *pos + len > bytes.len() {
            return Err("truncated native artifact string content".to_string());
        }
        let value = std::str::from_utf8(&bytes[*pos..*pos + len])
            .map_err(|_| "invalid utf-8 in native artifact".to_string())?
            .to_string();
        *pos += len;
        Ok(value)
    }

    pub(crate) fn native_recovery_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeRecoverySummary {
        const SAMPLE_LIMIT: usize = 16;

        let snapshots: Vec<_> = metadata
            .snapshots
            .iter()
            .rev()
            .take(SAMPLE_LIMIT)
            .map(|snapshot| NativeSnapshotSummary {
                snapshot_id: snapshot.snapshot_id,
                created_at_unix_ms: snapshot.created_at_unix_ms,
                superblock_sequence: snapshot.superblock_sequence,
                collection_count: snapshot.collection_count as u32,
                total_entities: snapshot.total_entities as u64,
            })
            .collect();
        let exports: Vec<_> = metadata
            .exports
            .iter()
            .rev()
            .take(SAMPLE_LIMIT)
            .map(|export| NativeExportSummary {
                name: export.name.clone(),
                created_at_unix_ms: export.created_at_unix_ms,
                snapshot_id: export.snapshot_id,
                superblock_sequence: export.superblock_sequence,
                collection_count: export.collection_count as u32,
                total_entities: export.total_entities as u64,
            })
            .collect();

        NativeRecoverySummary {
            snapshot_count: metadata.snapshots.len() as u32,
            export_count: metadata.exports.len() as u32,
            snapshots_complete: metadata.snapshots.len() <= SAMPLE_LIMIT,
            exports_complete: metadata.exports.len() <= SAMPLE_LIMIT,
            omitted_snapshot_count: metadata.snapshots.len().saturating_sub(snapshots.len()) as u32,
            omitted_export_count: metadata.exports.len().saturating_sub(exports.len()) as u32,
            snapshots,
            exports,
        }
    }

    pub(crate) fn native_catalog_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeCatalogSummary {
        const SAMPLE_LIMIT: usize = 32;

        let collections: Vec<_> = metadata
            .catalog
            .stats_by_collection
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|(name, stats)| NativeCatalogCollectionSummary {
                name: name.clone(),
                entities: stats.entities as u64,
                cross_refs: stats.cross_refs as u64,
                segments: stats.segments as u32,
            })
            .collect();

        NativeCatalogSummary {
            collection_count: metadata.catalog.total_collections as u32,
            total_entities: metadata.catalog.total_entities as u64,
            collections_complete: metadata.catalog.stats_by_collection.len() <= SAMPLE_LIMIT,
            omitted_collection_count: metadata
                .catalog
                .stats_by_collection
                .len()
                .saturating_sub(collections.len()) as u32,
            collections,
        }
    }

    pub(crate) fn native_metadata_state_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeMetadataStateSummary {
        NativeMetadataStateSummary {
            protocol_version: metadata.protocol_version.clone(),
            generated_at_unix_ms: metadata.generated_at_unix_ms,
            last_loaded_from: metadata.last_loaded_from.clone(),
            last_healed_at_unix_ms: metadata.last_healed_at_unix_ms,
        }
    }

    pub(crate) fn inspect_native_header_against_metadata(
        native: PhysicalFileHeader,
        metadata: &PhysicalMetadataFile,
    ) -> NativeHeaderInspection {
        let expected = Self::native_header_from_metadata(metadata);
        let mut mismatches = Vec::new();

        if native.format_version != expected.format_version {
            mismatches.push(NativeHeaderMismatch {
                field: "format_version",
                native: native.format_version.to_string(),
                expected: expected.format_version.to_string(),
            });
        }
        if native.sequence != expected.sequence {
            mismatches.push(NativeHeaderMismatch {
                field: "sequence",
                native: native.sequence.to_string(),
                expected: expected.sequence.to_string(),
            });
        }
        if native.manifest_oldest_root != expected.manifest_oldest_root {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_oldest_root",
                native: native.manifest_oldest_root.to_string(),
                expected: expected.manifest_oldest_root.to_string(),
            });
        }
        if native.manifest_root != expected.manifest_root {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_root",
                native: native.manifest_root.to_string(),
                expected: expected.manifest_root.to_string(),
            });
        }
        if native.free_set_root != expected.free_set_root {
            mismatches.push(NativeHeaderMismatch {
                field: "free_set_root",
                native: native.free_set_root.to_string(),
                expected: expected.free_set_root.to_string(),
            });
        }
        if native.collection_root_count != expected.collection_root_count {
            mismatches.push(NativeHeaderMismatch {
                field: "collection_root_count",
                native: native.collection_root_count.to_string(),
                expected: expected.collection_root_count.to_string(),
            });
        }
        if native.snapshot_count != expected.snapshot_count {
            mismatches.push(NativeHeaderMismatch {
                field: "snapshot_count",
                native: native.snapshot_count.to_string(),
                expected: expected.snapshot_count.to_string(),
            });
        }
        if native.index_count != expected.index_count {
            mismatches.push(NativeHeaderMismatch {
                field: "index_count",
                native: native.index_count.to_string(),
                expected: expected.index_count.to_string(),
            });
        }
        if native.catalog_collection_count != expected.catalog_collection_count {
            mismatches.push(NativeHeaderMismatch {
                field: "catalog_collection_count",
                native: native.catalog_collection_count.to_string(),
                expected: expected.catalog_collection_count.to_string(),
            });
        }
        if native.catalog_total_entities != expected.catalog_total_entities {
            mismatches.push(NativeHeaderMismatch {
                field: "catalog_total_entities",
                native: native.catalog_total_entities.to_string(),
                expected: expected.catalog_total_entities.to_string(),
            });
        }
        if native.export_count != expected.export_count {
            mismatches.push(NativeHeaderMismatch {
                field: "export_count",
                native: native.export_count.to_string(),
                expected: expected.export_count.to_string(),
            });
        }
        if native.graph_projection_count != expected.graph_projection_count {
            mismatches.push(NativeHeaderMismatch {
                field: "graph_projection_count",
                native: native.graph_projection_count.to_string(),
                expected: expected.graph_projection_count.to_string(),
            });
        }
        if native.analytics_job_count != expected.analytics_job_count {
            mismatches.push(NativeHeaderMismatch {
                field: "analytics_job_count",
                native: native.analytics_job_count.to_string(),
                expected: expected.analytics_job_count.to_string(),
            });
        }
        if native.manifest_event_count != expected.manifest_event_count {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_event_count",
                native: native.manifest_event_count.to_string(),
                expected: expected.manifest_event_count.to_string(),
            });
        }

        NativeHeaderInspection {
            native,
            expected,
            consistent: mismatches.is_empty(),
            mismatches,
        }
    }

    pub(crate) fn repair_policy_for_inspection(
        inspection: &NativeHeaderInspection,
    ) -> NativeHeaderRepairPolicy {
        if inspection.consistent {
            return NativeHeaderRepairPolicy::InSync;
        }

        if inspection.expected.sequence >= inspection.native.sequence {
            NativeHeaderRepairPolicy::RepairNativeFromMetadata
        } else {
            NativeHeaderRepairPolicy::NativeAheadOfMetadata
        }
    }

    pub(crate) fn prune_export_registry(&self, exports: &mut Vec<ExportDescriptor>) {
        let retention = self.options.export_retention.max(1);
        if exports.len() <= retention {
            return;
        }

        exports.sort_by_key(|export| export.created_at_unix_ms);
        let removed: Vec<ExportDescriptor> =
            exports.drain(0..(exports.len() - retention)).collect();

        for export in removed {
            let _ = fs::remove_file(&export.data_path);
            let _ = fs::remove_file(&export.metadata_path);
            let binary_path = PhysicalMetadataFile::metadata_binary_path_for(std::path::Path::new(
                &export.data_path,
            ));
            let _ = fs::remove_file(binary_path);
        }
    }

    pub(crate) fn runtime_index_catalog(&self) -> IndexCatalog {
        let mut catalog = IndexCatalog::register_default_vector_graph(
            self.options.has_capability(Capability::Table),
            self.options.has_capability(Capability::Graph),
        );
        if self.options.has_capability(Capability::FullText) {
            catalog.register(RuntimeIndexConfig::new(
                "text-fulltext",
                IndexKind::FullText,
            ));
            catalog.register(RuntimeIndexConfig::new(
                "document-pathvalue",
                IndexKind::DocumentPathValue,
            ));
        }
        catalog.register(RuntimeIndexConfig::new(
            "search-hybrid",
            IndexKind::HybridSearch,
        ));
        catalog
    }

    pub(crate) fn physical_index_state(&self) -> Vec<PhysicalIndexState> {
        // Use a lightweight catalog snapshot that does NOT call physical_metadata()
        // to avoid infinite recursion: physical_metadata → metadata_from_native_state
        // → physical_index_state → catalog_model_snapshot → physical_metadata → ...
        let catalog = self.runtime_index_catalog();
        let snapshot = crate::catalog::snapshot_store_with_declarations(
            "reddb",
            self.store.as_ref(),
            Some(&catalog),
            None, // No declarations — breaks the recursive cycle
        );
        let mut metrics_by_name = std::collections::BTreeMap::new();
        for metric in &snapshot.indices {
            metrics_by_name.insert(metric.name.clone(), metric.clone());
        }

        let mut states = Vec::new();
        for collection in snapshot.collections {
            for index_name in &collection.indices {
                let metric = metrics_by_name.get(index_name);
                let kind = metric
                    .map(|metric| metric.kind)
                    .unwrap_or_else(|| infer_collection_index_kind(collection.model, index_name));
                let entries = estimate_index_entries(&collection, kind);
                states.push(PhysicalIndexState {
                    name: format!("{}::{}", collection.name, index_name),
                    kind,
                    collection: Some(collection.name.clone()),
                    enabled: metric.map(|metric| metric.enabled).unwrap_or(true),
                    entries,
                    estimated_memory_bytes: estimate_index_memory(entries, kind),
                    last_refresh_ms: metric.and_then(|metric| metric.last_refresh_ms),
                    backend: index_backend_name(kind).to_string(),
                    artifact_kind: None,
                    artifact_root_page: None,
                    artifact_checksum: None,
                    build_state: "catalog-derived".to_string(),
                });
            }
        }

        states
    }

    pub(crate) fn physical_collection_roots(&self) -> BTreeMap<String, u64> {
        let mut roots = BTreeMap::new();

        for name in self.store.list_collections() {
            let Some(manager) = self.store.get_collection(&name) else {
                continue;
            };

            let stats = manager.stats();
            let mut root = fnv1a_seed();
            fnv1a_hash_value(&mut root, &name);
            fnv1a_hash_value(&mut root, &stats.total_entities);
            fnv1a_hash_value(&mut root, &stats.growing_count);
            fnv1a_hash_value(&mut root, &stats.sealed_count);
            fnv1a_hash_value(&mut root, &stats.archived_count);
            fnv1a_hash_value(&mut root, &stats.total_memory_bytes);
            fnv1a_hash_value(&mut root, &stats.seal_ops);
            fnv1a_hash_value(&mut root, &stats.compact_ops);

            let mut entities = manager.query_all(|_| true);
            entities.sort_by_key(|entity| entity.id.raw());

            for entity in entities {
                fnv1a_hash_value(&mut root, &entity.id.raw());
                fnv1a_hash_value(&mut root, &entity.kind);
                fnv1a_hash_value(&mut root, &entity.created_at);
                fnv1a_hash_value(&mut root, &entity.updated_at);
                fnv1a_hash_value(&mut root, &entity.data);
                fnv1a_hash_value(&mut root, &entity.sequence_id);
                fnv1a_hash_value(&mut root, &entity.embeddings().len());
                fnv1a_hash_value(&mut root, &entity.cross_refs().len());
            }

            roots.insert(name, root);
        }

        roots
    }

    // ========================================================================
    // Reference Helpers - For Metadata Linking
    // ========================================================================

    /// Create a reference to a table row
    pub fn table_ref(&self, table: impl Into<String>, row_id: u64) -> TableRef {
        TableRef::new(table, row_id)
    }

    /// Create a reference to a graph node
    pub fn node_ref(&self, collection: impl Into<String>, node_id: EntityId) -> NodeRef {
        NodeRef::new(collection, node_id)
    }

    /// Create a reference to a vector
    pub fn vector_ref(&self, collection: impl Into<String>, vector_id: EntityId) -> VectorRef {
        VectorRef::new(collection, vector_id)
    }

    // ========================================================================
    // Query API
    // ========================================================================

    /// Start building a query
    pub fn query(&self) -> QueryBuilder {
        QueryBuilder::new(self.store.clone())
    }

    /// Quick vector similarity search.
    ///
    /// For collections with >= 100 vectors a lazily-built HNSW index is used
    /// for fast approximate nearest-neighbor lookup.  Smaller collections fall
    /// back to an exact brute-force scan so that the overhead of building an
    /// index is avoided when it would not pay off.
    pub fn similar(&self, collection: &str, vector: &[f32], k: usize) -> Vec<SimilarResult> {
        if self.store.get_collection(collection).is_none() {
            return Vec::new();
        }

        // Try the HNSW fast path for collections with enough vectors.
        if let Some(index) = self.get_or_build_hnsw_index(collection, vector.len()) {
            let hnsw = index.read().unwrap_or_else(|e| e.into_inner());
            let results = hnsw.search(vector, k);
            let mapped = self.hnsw_results_to_similar(collection, &results);
            if !mapped.is_empty() {
                return mapped;
            }
        }

        // Fallback: brute-force scan (small / mixed-type collections).
        self.similar_brute_force(collection, vector, k)
    }

    /// Brute-force cosine similarity scan (exact results, O(n)).
    fn similar_brute_force(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
    ) -> Vec<SimilarResult> {
        let manager = match self.store.get_collection(collection) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let entities = manager.query_all(|_| true);
        let mut results: Vec<SimilarResult> = entities
            .iter()
            .filter_map(|e| {
                let score = match &e.data {
                    EntityData::Vector(v) => cosine_similarity(vector, &v.dense),
                    _ => e
                        .embeddings()
                        .iter()
                        .map(|emb| cosine_similarity(vector, &emb.vector))
                        .fold(0.0f32, f32::max),
                };
                let distance = (1.0 - score).max(0.0);
                if score > 0.0 {
                    Some(SimilarResult {
                        entity_id: e.id,
                        score,
                        distance,
                        entity: e.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);
        results
    }

    /// Return (or lazily build) a per-collection HNSW index.
    ///
    /// Returns `None` when the collection has fewer than 100 dense vectors
    /// (the brute-force path is cheaper for tiny collections) or when there
    /// is a dimension mismatch with the query vector.
    ///
    /// The cached index is automatically invalidated when the live entity
    /// count in the collection changes, so inserts and deletes are picked up
    /// transparently without requiring explicit invalidation calls.
    fn get_or_build_hnsw_index(
        &self,
        collection: &str,
        query_dim: usize,
    ) -> Option<Arc<RwLock<HnswIndex>>> {
        let manager = self.store.get_collection(collection)?;
        let live_count = manager.count();

        // Fast path: check if a fresh index already exists.
        {
            let indexes = self
                .vector_indexes
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = indexes.get(collection) {
                if cached.entity_count == live_count {
                    return Some(Arc::clone(&cached.index));
                }
            }
        }

        // Either no cached index exists or it is stale -- (re)build it.
        let entities = manager.query_all(|_| true);

        let vectors: Vec<(u64, Vec<f32>)> = entities
            .iter()
            .filter_map(|e| match &e.data {
                EntityData::Vector(v) if !v.dense.is_empty() && v.dense.len() == query_dim => {
                    Some((e.id.raw(), v.dense.clone()))
                }
                _ => None,
            })
            .collect();

        // Only build the HNSW index when there are enough vectors to justify it.
        const MIN_VECTORS_FOR_HNSW: usize = 100;
        if vectors.len() < MIN_VECTORS_FOR_HNSW {
            return None;
        }

        // Build the HNSW index with cosine distance (matching the brute-force path).
        let config = crate::storage::engine::HnswConfig::with_m(16)
            .with_metric(crate::storage::engine::DistanceMetric::Cosine)
            .with_ef_construction(100)
            .with_ef_search(50);
        let mut hnsw = HnswIndex::new(query_dim, config);

        for (id, vec) in &vectors {
            hnsw.insert_with_id(*id, vec.clone());
        }

        let index = Arc::new(RwLock::new(hnsw));

        // Store in the cache (double-check pattern to avoid duplicate builds).
        let mut indexes = self
            .vector_indexes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        // Re-check under write lock: another thread may have built in the meantime.
        if let Some(cached) = indexes.get(collection) {
            if cached.entity_count == live_count {
                return Some(Arc::clone(&cached.index));
            }
        }
        indexes.insert(
            collection.to_string(),
            CachedVectorIndex {
                index: Arc::clone(&index),
                entity_count: live_count,
            },
        );
        Some(index)
    }

    /// Convert HNSW `DistanceResult`s back to the DevX `SimilarResult` type.
    fn hnsw_results_to_similar(
        &self,
        collection: &str,
        results: &[crate::storage::engine::DistanceResult],
    ) -> Vec<SimilarResult> {
        results
            .iter()
            .filter_map(|dr| {
                let entity_id = EntityId::new(dr.id);
                let entity = self.store.get(collection, entity_id)?;
                // Cosine distance = 1 - similarity.
                let score = (1.0 - dr.distance).max(0.0);
                if score > 0.0 {
                    Some(SimilarResult {
                        entity_id,
                        score,
                        distance: dr.distance,
                        entity,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Invalidate the cached HNSW index for a collection.
    ///
    /// Called after vector inserts / deletes so the next search lazily rebuilds
    /// a fresh index that includes the new data.
    pub(crate) fn invalidate_vector_index(&self, collection: &str) {
        let mut indexes = self
            .vector_indexes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        indexes.remove(collection);
    }

    /// Get entity by ID from any collection
    pub fn get(&self, id: EntityId) -> Option<UnifiedEntity> {
        self.store.get_any(id).map(|(_, e)| e)
    }

    /// Get entity with its collection name
    pub fn get_with_collection(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        self.store.get_any(id)
    }

    // ========================================================================
    // Batch Operations - Performance
    // ========================================================================

    /// Batch get multiple entities by ID. More efficient than N individual get() calls.
    pub fn batch_get(&self, ids: &[EntityId]) -> Vec<Option<UnifiedEntity>> {
        ids.iter().map(|id| self.get(*id)).collect()
    }

    /// Start a batch operation for bulk inserts
    pub fn batch(&self) -> BatchBuilder {
        BatchBuilder::new(self.store.clone())
    }

    // ========================================================================
    // Preprocessing
    // ========================================================================

    /// Add a preprocessor hook
    pub fn add_preprocessor(&mut self, preprocessor: Box<dyn Preprocessor>) {
        self.preprocessors.push(preprocessor);
    }

    /// Run preprocessors on an entity
    #[allow(dead_code)]
    fn preprocess(&self, entity: &mut UnifiedEntity) {
        for preprocessor in &self.preprocessors {
            preprocessor.process(entity);
        }
    }

    // ========================================================================
    // Cross-Reference Navigation
    // ========================================================================

    /// Get all entities linked FROM the given entity
    pub fn linked_from(&self, id: EntityId) -> Vec<LinkedEntity> {
        self.store
            .get_refs_from(id)
            .into_iter()
            .filter_map(|(target_id, ref_type, collection)| {
                self.store
                    .get(&collection, target_id)
                    .map(|entity| LinkedEntity {
                        entity,
                        ref_type,
                        collection,
                    })
            })
            .collect()
    }

    /// Get all entities linked TO the given entity
    pub fn linked_to(&self, id: EntityId) -> Vec<LinkedEntity> {
        self.store
            .get_refs_to(id)
            .into_iter()
            .filter_map(|(source_id, ref_type, collection)| {
                self.store
                    .get(&collection, source_id)
                    .map(|entity| LinkedEntity {
                        entity,
                        ref_type,
                        collection,
                    })
            })
            .collect()
    }

    /// Get the underlying store (for advanced operations)
    pub fn store(&self) -> Arc<UnifiedStore> {
        self.store.clone()
    }

    pub(crate) fn is_binary_dump(path: &Path) -> Result<bool, std::io::Error> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 4];
        let read = file.read(&mut magic)?;
        Ok(read == 4 && &magic == b"RDST")
    }
}
