use super::*;

impl UnifiedStore {
    pub fn new() -> Self {
        Self::with_config(UnifiedStoreConfig::default())
    }

    /// Get the current storage format version
    pub fn format_version(&self) -> u32 {
        self.format_version.load(Ordering::SeqCst)
    }

    pub(crate) fn set_format_version(&self, version: u32) {
        self.format_version.store(version, Ordering::SeqCst);
    }

    /// Allocate a global entity ID
    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    pub(crate) fn register_entity_id(&self, id: EntityId) {
        let candidate = id.raw().saturating_add(1);
        let mut current = self.next_entity_id.load(Ordering::SeqCst);
        while candidate > current {
            match self.next_entity_id.compare_exchange(
                current,
                candidate,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    /// Load store from binary file
    ///
    /// Binary format:
    /// ```text
    /// [magic: 4 bytes "RDST"]
    /// [version: u32]
    /// [collection_count: varu32]
    /// [collections...]
    /// [cross_ref_count: varu32]
    /// [cross_refs...]
    /// ```
    pub fn load_from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        // Verify magic bytes "RDST" (RedDB Store)
        if buf.len() < 8 {
            return Err("File too small".into());
        }
        if &buf[0..4] != STORE_MAGIC {
            return Err("Invalid magic bytes - expected RDST".into());
        }
        let mut pos = 4;

        // Version check
        let version = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        pos += 4;
        if version != STORE_VERSION_V1 && version != STORE_VERSION_V2 && version != STORE_VERSION_V3
        {
            return Err(format!("Unsupported version: {}", version).into());
        }

        // V3+ has CRC32 footer — verify integrity before parsing
        if version >= STORE_VERSION_V3 {
            if buf.len() < 12 {
                return Err("File too small for CRC32 verification".into());
            }
            let stored_crc = u32::from_le_bytes([
                buf[buf.len() - 4],
                buf[buf.len() - 3],
                buf[buf.len() - 2],
                buf[buf.len() - 1],
            ]);
            let computed_crc = crate::storage::engine::crc32::crc32(&buf[..buf.len() - 4]);
            if stored_crc != computed_crc {
                return Err("Binary store CRC32 mismatch — file corrupted".into());
            }
            // Trim the CRC footer so parsing doesn't read into it
            buf.truncate(buf.len() - 4);
        }

        let store = Self::with_config(UnifiedStoreConfig::default());
        store.set_format_version(version);

        // Read collection count
        let collection_count = read_varu32(&buf, &mut pos)
            .map_err(|e| format!("Failed to read collection count: {:?}", e))?;

        // Read each collection
        for _ in 0..collection_count {
            // Collection name
            let name_len = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read name length: {:?}", e))?
                as usize;
            let name = String::from_utf8(buf[pos..pos + name_len].to_vec())
                .map_err(|e| format!("Invalid UTF-8 in collection name: {}", e))?;
            pos += name_len;

            // Entity count
            let entity_count = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read entity count: {:?}", e))?;

            // Read each entity
            for _ in 0..entity_count {
                let entity = Self::read_entity_binary(&buf, &mut pos, version)?;
                store.insert_auto(&name, entity)?;
            }
        }

        if pos < buf.len() {
            // Read cross-references section
            let cross_ref_count = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read cross-ref count: {:?}", e))?;

            for _ in 0..cross_ref_count {
                let source_id = read_varu64(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read source_id: {:?}", e))?;
                let target_id = read_varu64(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read target_id: {:?}", e))?;
                let ref_type_byte = buf[pos];
                pos += 1;
                let ref_type = RefType::from_byte(ref_type_byte);

                let coll_len = read_varu32(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read collection length: {:?}", e))?
                    as usize;
                let collection = String::from_utf8(buf[pos..pos + coll_len].to_vec())
                    .map_err(|e| format!("Invalid UTF-8 in collection: {}", e))?;
                pos += coll_len;

                let source_collection = store
                    .get_any(EntityId::new(source_id))
                    .map(|(name, _)| name)
                    .unwrap_or_else(|| collection.clone());
                let _ = store.add_cross_ref(
                    &source_collection,
                    EntityId::new(source_id),
                    &collection,
                    EntityId::new(target_id),
                    ref_type,
                    1.0,
                );
            }
        }

        Ok(store)
    }

    /// Save store to binary file
    ///
    /// Uses compact binary encoding with varint for efficient storage.
    /// No JSON - pure binary with pages and indices.
    pub fn save_to_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        // Write to temp file first, then atomic rename
        let tmp_path = path.with_extension("rdb-tmp");
        let file = File::create(&tmp_path)?;
        let mut writer = BufWriter::new(file);
        let mut buf = Vec::new();

        // Magic bytes "RDST"
        buf.extend_from_slice(STORE_MAGIC);

        // Version (3 — includes CRC32 footer)
        buf.extend_from_slice(&STORE_VERSION_V3.to_le_bytes());

        // Get all collections
        let collections = self
            .collections
            .read()
            .map_err(|_| -> Box<dyn std::error::Error> { "collections lock poisoned".into() })?;
        write_varu32(&mut buf, collections.len() as u32);

        for (name, manager) in collections.iter() {
            // Collection name
            write_varu32(&mut buf, name.len() as u32);
            buf.extend_from_slice(name.as_bytes());

            // Get all entities from this collection
            let entities = manager.query_all(|_| true);
            write_varu32(&mut buf, entities.len() as u32);

            for entity in entities {
                Self::write_entity_binary(&mut buf, &entity, STORE_VERSION_V2);
            }
        }

        // Write cross-references
        let cross_refs = self
            .cross_refs
            .read()
            .map_err(|_| -> Box<dyn std::error::Error> { "cross_refs lock poisoned".into() })?;
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        write_varu32(&mut buf, total_refs as u32);

        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                write_varu64(&mut buf, source_id.raw());
                write_varu64(&mut buf, target_id.raw());
                buf.push(ref_type.to_byte());
                write_varu32(&mut buf, collection.len() as u32);
                buf.extend_from_slice(collection.as_bytes());
            }
        }

        self.set_format_version(STORE_VERSION_V3);

        // Append CRC32 footer over entire content
        let checksum = crate::storage::engine::crc32::crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());

        writer.write_all(&buf)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);

        // Atomic rename: tmp → final
        std::fs::rename(&tmp_path, path)?;

        // fsync parent directory for rename durability
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        Ok(())
    }

    /// Read entity from binary buffer
    pub(crate) fn read_entity_binary(
        buf: &[u8],
        pos: &mut usize,
        format_version: u32,
    ) -> Result<UnifiedEntity, Box<dyn std::error::Error>> {
        // Entity ID
        let id = read_varu64(buf, pos).map_err(|e| format!("Failed to read entity id: {:?}", e))?;

        // EntityKind type byte
        let kind_type = buf[*pos];
        *pos += 1;

        // EntityKind details
        let kind = match kind_type {
            0 => {
                // TableRow
                let table_len = Self::read_varu32_safe(buf, pos)?;
                let table = String::from_utf8(buf[*pos..*pos + table_len].to_vec())?;
                *pos += table_len;
                let row_id = Self::read_varu64_safe(buf, pos)?;
                EntityKind::TableRow { table, row_id }
            }
            1 => {
                // GraphNode
                let label_len = Self::read_varu32_safe(buf, pos)?;
                let label = String::from_utf8(buf[*pos..*pos + label_len].to_vec())?;
                *pos += label_len;
                let node_type_len = Self::read_varu32_safe(buf, pos)?;
                let node_type = String::from_utf8(buf[*pos..*pos + node_type_len].to_vec())?;
                *pos += node_type_len;
                EntityKind::GraphNode { label, node_type }
            }
            2 => {
                // GraphEdge
                let label_len = Self::read_varu32_safe(buf, pos)?;
                let label = String::from_utf8(buf[*pos..*pos + label_len].to_vec())?;
                *pos += label_len;
                let from_node_len = Self::read_varu32_safe(buf, pos)?;
                let from_node = String::from_utf8(buf[*pos..*pos + from_node_len].to_vec())?;
                *pos += from_node_len;
                let to_node_len = Self::read_varu32_safe(buf, pos)?;
                let to_node = String::from_utf8(buf[*pos..*pos + to_node_len].to_vec())?;
                *pos += to_node_len;
                let weight =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                EntityKind::GraphEdge {
                    label,
                    from_node,
                    to_node,
                    weight,
                }
            }
            3 => {
                // Vector
                let collection_len = Self::read_varu32_safe(buf, pos)?;
                let collection = String::from_utf8(buf[*pos..*pos + collection_len].to_vec())?;
                *pos += collection_len;
                EntityKind::Vector { collection }
            }
            _ => return Err(format!("Unknown EntityKind type: {}", kind_type).into()),
        };

        // EntityData type byte
        let data_type = buf[*pos];
        *pos += 1;

        // EntityData
        let data = match data_type {
            0 => {
                // Row
                let col_count = Self::read_varu32_safe(buf, pos)?;
                let mut columns = Vec::with_capacity(col_count);
                for _ in 0..col_count {
                    columns.push(Self::read_value_binary(buf, pos)?);
                }
                EntityData::Row(RowData::new(columns))
            }
            1 => {
                // Node
                let prop_count = Self::read_varu32_safe(buf, pos)?;
                let mut properties = HashMap::new();
                for _ in 0..prop_count {
                    let key_len = Self::read_varu32_safe(buf, pos)?;
                    let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                    *pos += key_len;
                    let value = Self::read_value_binary(buf, pos)?;
                    properties.insert(key, value);
                }
                EntityData::Node(NodeData::with_properties(properties))
            }
            2 => {
                // Edge
                let weight_bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                let weight = f32::from_le_bytes(weight_bytes);
                let prop_count = Self::read_varu32_safe(buf, pos)?;
                let mut properties = HashMap::new();
                for _ in 0..prop_count {
                    let key_len = Self::read_varu32_safe(buf, pos)?;
                    let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                    *pos += key_len;
                    let value = Self::read_value_binary(buf, pos)?;
                    properties.insert(key, value);
                }
                let mut edge = EdgeData::new(weight);
                edge.properties = properties;
                EntityData::Edge(edge)
            }
            3 => {
                // Vector
                let dim = Self::read_varu32_safe(buf, pos)?;
                let mut dense = Vec::with_capacity(dim);
                for _ in 0..dim {
                    let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    dense.push(f32::from_le_bytes(bytes));
                }
                EntityData::Vector(VectorData::new(dense))
            }
            6 => {
                // Row with named HashMap
                let field_count = Self::read_varu32_safe(buf, pos)?;
                let mut named = HashMap::with_capacity(field_count);
                for _ in 0..field_count {
                    let key_len = Self::read_varu32_safe(buf, pos)?;
                    let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                    *pos += key_len;
                    let value = Self::read_value_binary(buf, pos)?;
                    named.insert(key, value);
                }
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(named),
                    schema: None,
                })
            }
            _ => return Err(format!("Unknown EntityData type: {}", data_type).into()),
        };

        // Timestamps
        let created_at = Self::read_varu64_safe(buf, pos)?;
        let updated_at = Self::read_varu64_safe(buf, pos)?;

        // Embeddings count
        let embedding_count = Self::read_varu32_safe(buf, pos)?;
        let mut embeddings = Vec::with_capacity(embedding_count);
        for _ in 0..embedding_count {
            let name_len = Self::read_varu32_safe(buf, pos)?;
            let name = String::from_utf8(buf[*pos..*pos + name_len].to_vec())?;
            *pos += name_len;

            let dim = Self::read_varu32_safe(buf, pos)?;
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                vector.push(f32::from_le_bytes(bytes));
            }

            let model_len = Self::read_varu32_safe(buf, pos)?;
            let model = String::from_utf8(buf[*pos..*pos + model_len].to_vec())?;
            *pos += model_len;

            embeddings.push(EmbeddingSlot::new(name, vector, model));
        }

        // Cross-refs count
        let cross_ref_count = Self::read_varu32_safe(buf, pos)?;
        let mut cross_refs = Vec::with_capacity(cross_ref_count);
        for _ in 0..cross_ref_count {
            let source = Self::read_varu64_safe(buf, pos)?;
            let target = Self::read_varu64_safe(buf, pos)?;
            let ref_type_byte = buf[*pos];
            *pos += 1;
            let (target_collection, weight, created_at) = if format_version >= STORE_VERSION_V2 {
                let coll_len = Self::read_varu32_safe(buf, pos)?;
                let collection = String::from_utf8(buf[*pos..*pos + coll_len].to_vec())?;
                *pos += coll_len;
                let weight_bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                let weight = f32::from_le_bytes(weight_bytes);
                let created_at = Self::read_varu64_safe(buf, pos)?;
                (collection, weight, created_at)
            } else {
                (String::new(), 1.0, 0)
            };

            let mut cross_ref = CrossRef::new(
                EntityId::new(source),
                EntityId::new(target),
                target_collection,
                RefType::from_byte(ref_type_byte),
            );
            cross_ref.weight = weight;
            cross_ref.created_at = created_at;
            cross_refs.push(cross_ref);
        }

        // Sequence ID
        let sequence_id = Self::read_varu64_safe(buf, pos)?;

        let entity = UnifiedEntity {
            id: EntityId::new(id),
            kind,
            created_at,
            updated_at,
            data,
            embeddings,
            cross_refs,
            sequence_id,
        };

        Ok(entity)
    }

    /// Safe varu32 reader that converts DecodeError to Box<dyn Error>
    fn read_varu32_safe(buf: &[u8], pos: &mut usize) -> Result<usize, Box<dyn std::error::Error>> {
        read_varu32(buf, pos)
            .map(|v| v as usize)
            .map_err(|e| format!("Decode error: {:?}", e).into())
    }

    /// Safe varu64 reader that converts DecodeError to Box<dyn Error>
    fn read_varu64_safe(buf: &[u8], pos: &mut usize) -> Result<u64, Box<dyn std::error::Error>> {
        read_varu64(buf, pos).map_err(|e| format!("Decode error: {:?}", e).into())
    }

    /// Write entity to binary buffer
    pub(crate) fn write_entity_binary(
        buf: &mut Vec<u8>,
        entity: &UnifiedEntity,
        format_version: u32,
    ) {
        // Entity ID
        write_varu64(buf, entity.id.raw());

        // EntityKind
        match &entity.kind {
            EntityKind::TableRow { table, row_id } => {
                buf.push(0);
                write_varu32(buf, table.len() as u32);
                buf.extend_from_slice(table.as_bytes());
                write_varu64(buf, *row_id);
            }
            EntityKind::GraphNode { label, node_type } => {
                buf.push(1);
                write_varu32(buf, label.len() as u32);
                buf.extend_from_slice(label.as_bytes());
                write_varu32(buf, node_type.len() as u32);
                buf.extend_from_slice(node_type.as_bytes());
            }
            EntityKind::GraphEdge {
                label,
                from_node,
                to_node,
                weight,
            } => {
                buf.push(2);
                write_varu32(buf, label.len() as u32);
                buf.extend_from_slice(label.as_bytes());
                write_varu32(buf, from_node.len() as u32);
                buf.extend_from_slice(from_node.as_bytes());
                write_varu32(buf, to_node.len() as u32);
                buf.extend_from_slice(to_node.as_bytes());
                buf.extend_from_slice(&weight.to_le_bytes());
            }
            EntityKind::Vector { collection } => {
                buf.push(3);
                write_varu32(buf, collection.len() as u32);
                buf.extend_from_slice(collection.as_bytes());
            }
            EntityKind::TimeSeriesPoint { series, metric } => {
                buf.push(4);
                write_varu32(buf, series.len() as u32);
                buf.extend_from_slice(series.as_bytes());
                write_varu32(buf, metric.len() as u32);
                buf.extend_from_slice(metric.as_bytes());
            }
            EntityKind::QueueMessage { queue, position } => {
                buf.push(5);
                write_varu32(buf, queue.len() as u32);
                buf.extend_from_slice(queue.as_bytes());
                write_varu64(buf, *position);
            }
        }

        // EntityData
        match &entity.data {
            EntityData::Row(row) => {
                if let Some(ref named) = row.named {
                    // Named row: type 6 = Row with named HashMap
                    buf.push(6);
                    write_varu32(buf, named.len() as u32);
                    for (key, value) in named {
                        write_varu32(buf, key.len() as u32);
                        buf.extend_from_slice(key.as_bytes());
                        Self::write_value_binary(buf, value);
                    }
                } else {
                    buf.push(0);
                    write_varu32(buf, row.columns.len() as u32);
                    for col in &row.columns {
                        Self::write_value_binary(buf, col);
                    }
                }
            }
            EntityData::Node(node) => {
                buf.push(1);
                write_varu32(buf, node.properties.len() as u32);
                for (key, value) in &node.properties {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    Self::write_value_binary(buf, value);
                }
            }
            EntityData::Edge(edge) => {
                buf.push(2);
                buf.extend_from_slice(&edge.weight.to_le_bytes());
                write_varu32(buf, edge.properties.len() as u32);
                for (key, value) in &edge.properties {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    Self::write_value_binary(buf, value);
                }
            }
            EntityData::Vector(vec) => {
                buf.push(3);
                write_varu32(buf, vec.dense.len() as u32);
                for f in &vec.dense {
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
            EntityData::TimeSeries(ts) => {
                buf.push(4);
                write_varu32(buf, ts.metric.len() as u32);
                buf.extend_from_slice(ts.metric.as_bytes());
                write_varu64(buf, ts.timestamp_ns);
                buf.extend_from_slice(&ts.value.to_le_bytes());
            }
            EntityData::QueueMessage(msg) => {
                buf.push(5);
                Self::write_value_binary(buf, &msg.payload);
                write_varu64(buf, msg.enqueued_at_ns);
                write_varu32(buf, msg.attempts);
            }
        }

        // Timestamps
        write_varu64(buf, entity.created_at);
        write_varu64(buf, entity.updated_at);

        // Embeddings
        write_varu32(buf, entity.embeddings.len() as u32);
        for emb in &entity.embeddings {
            write_varu32(buf, emb.name.len() as u32);
            buf.extend_from_slice(emb.name.as_bytes());
            write_varu32(buf, emb.vector.len() as u32);
            for f in &emb.vector {
                buf.extend_from_slice(&f.to_le_bytes());
            }
            write_varu32(buf, emb.model.len() as u32);
            buf.extend_from_slice(emb.model.as_bytes());
        }

        // Cross-refs
        write_varu32(buf, entity.cross_refs.len() as u32);
        for cross_ref in &entity.cross_refs {
            write_varu64(buf, cross_ref.source.raw());
            write_varu64(buf, cross_ref.target.raw());
            buf.push(cross_ref.ref_type.to_byte());
            if format_version >= STORE_VERSION_V2 {
                write_varu32(buf, cross_ref.target_collection.len() as u32);
                buf.extend_from_slice(cross_ref.target_collection.as_bytes());
                buf.extend_from_slice(&cross_ref.weight.to_le_bytes());
                write_varu64(buf, cross_ref.created_at);
            }
        }

        // Sequence ID
        write_varu64(buf, entity.sequence_id);
    }

    /// Read a Value from binary buffer
    /// Type bytes: 0=Null, 1=Boolean, 2=Integer, 3=UnsignedInteger, 4=Float,
    /// 5=Text, 6=Blob, 7=Timestamp, 8=Duration, 9=IpAddr, 10=MacAddr,
    /// 11=Vector, 12=Json, 13=Uuid, 14=NodeRef, 15=EdgeRef, 16=VectorRef, 17=RowRef
    fn read_value_binary(buf: &[u8], pos: &mut usize) -> Result<Value, Box<dyn std::error::Error>> {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

        let type_byte = buf[*pos];
        *pos += 1;

        Ok(match type_byte {
            0 => Value::Null,
            1 => {
                let b = buf[*pos] != 0;
                *pos += 1;
                Value::Boolean(b)
            }
            2 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Integer(val)
            }
            3 => {
                let val = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::UnsignedInteger(val)
            }
            4 => {
                let val = f64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Float(val)
            }
            5 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::Text(s)
            }
            6 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let bytes = buf[*pos..*pos + len].to_vec();
                *pos += len;
                Value::Blob(bytes)
            }
            7 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Timestamp(val)
            }
            8 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Duration(val)
            }
            9 => {
                // IpAddr: first byte = version (4 or 6)
                let version = buf[*pos];
                *pos += 1;
                if version == 4 {
                    let octets = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    Value::IpAddr(IpAddr::V4(Ipv4Addr::from(octets)))
                } else {
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(&buf[*pos..*pos + 16]);
                    *pos += 16;
                    Value::IpAddr(IpAddr::V6(Ipv6Addr::from(octets)))
                }
            }
            10 => {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&buf[*pos..*pos + 6]);
                *pos += 6;
                Value::MacAddr(mac)
            }
            11 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let mut vector = Vec::with_capacity(len);
                for _ in 0..len {
                    let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    vector.push(f32::from_le_bytes(bytes));
                }
                Value::Vector(vector)
            }
            12 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let bytes = buf[*pos..*pos + len].to_vec();
                *pos += len;
                Value::Json(bytes)
            }
            13 => {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&buf[*pos..*pos + 16]);
                *pos += 16;
                Value::Uuid(uuid)
            }
            14 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::NodeRef(s)
            }
            15 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::EdgeRef(s)
            }
            16 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                let id = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::VectorRef(s, id)
            }
            17 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                let id = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::RowRef(s, id)
            }
            18 => {
                let rgb = [buf[*pos], buf[*pos + 1], buf[*pos + 2]];
                *pos += 3;
                Value::Color(rgb)
            }
            19 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::Email(s)
            }
            20 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::Url(s)
            }
            21 => {
                let val = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Phone(val)
            }
            22 => {
                let val =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Semver(val)
            }
            23 => {
                let ip =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                let prefix = buf[*pos];
                *pos += 1;
                Value::Cidr(ip, prefix)
            }
            24 => {
                let val =
                    i32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Date(val)
            }
            25 => {
                let val =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Time(val)
            }
            26 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Decimal(val)
            }
            27 => {
                let val = buf[*pos];
                *pos += 1;
                Value::EnumValue(val)
            }
            28 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let mut elems = Vec::with_capacity(len);
                for _ in 0..len {
                    elems.push(Self::read_value_binary(buf, pos)?);
                }
                Value::Array(elems)
            }
            29 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::TimestampMs(val)
            }
            30 => {
                let val =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Ipv4(val)
            }
            31 => {
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&buf[*pos..*pos + 16]);
                *pos += 16;
                Value::Ipv6(bytes)
            }
            32 => {
                let ip =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                let mask =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Subnet(ip, mask)
            }
            33 => {
                let val = u16::from_le_bytes([buf[*pos], buf[*pos + 1]]);
                *pos += 2;
                Value::Port(val)
            }
            34 => {
                let val =
                    i32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Latitude(val)
            }
            35 => {
                let val =
                    i32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::Longitude(val)
            }
            36 => {
                let lat =
                    i32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                let lon =
                    i32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::GeoPoint(lat, lon)
            }
            37 => {
                let c = [buf[*pos], buf[*pos + 1]];
                *pos += 2;
                Value::Country2(c)
            }
            38 => {
                let c = [buf[*pos], buf[*pos + 1], buf[*pos + 2]];
                *pos += 3;
                Value::Country3(c)
            }
            39 => {
                let c = [buf[*pos], buf[*pos + 1]];
                *pos += 2;
                Value::Lang2(c)
            }
            40 => {
                let c = [
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                ];
                *pos += 5;
                Value::Lang5(c)
            }
            41 => {
                let c = [buf[*pos], buf[*pos + 1], buf[*pos + 2]];
                *pos += 3;
                Value::Currency(c)
            }
            42 => {
                let rgba = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                Value::ColorAlpha(rgba)
            }
            43 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::BigInt(val)
            }
            44 => {
                let col_len = Self::read_varu32_safe(buf, pos)?;
                let col = String::from_utf8(buf[*pos..*pos + col_len].to_vec())?;
                *pos += col_len;
                let key_len = Self::read_varu32_safe(buf, pos)?;
                let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                *pos += key_len;
                Value::KeyRef(col, key)
            }
            45 => {
                let col_len = Self::read_varu32_safe(buf, pos)?;
                let col = String::from_utf8(buf[*pos..*pos + col_len].to_vec())?;
                *pos += col_len;
                let id = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::DocRef(col, id)
            }
            46 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let name = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::TableRef(name)
            }
            47 => {
                let val =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                Value::PageRef(val)
            }
            _ => return Err(format!("Unknown Value type: {}", type_byte).into()),
        })
    }

    /// Write a Value to binary buffer
    /// Type bytes: 0=Null, 1=Boolean, 2=Integer, 3=UnsignedInteger, 4=Float,
    /// 5=Text, 6=Blob, 7=Timestamp, 8=Duration, 9=IpAddr, 10=MacAddr,
    /// 11=Vector, 12=Json, 13=Uuid, 14=NodeRef, 15=EdgeRef, 16=VectorRef, 17=RowRef
    fn write_value_binary(buf: &mut Vec<u8>, value: &Value) {
        use std::net::IpAddr;

        match value {
            Value::Null => buf.push(0),
            Value::Boolean(b) => {
                buf.push(1);
                buf.push(if *b { 1 } else { 0 });
            }
            Value::Integer(i) => {
                buf.push(2);
                buf.extend_from_slice(&i.to_le_bytes());
            }
            Value::UnsignedInteger(u) => {
                buf.push(3);
                buf.extend_from_slice(&u.to_le_bytes());
            }
            Value::Float(f) => {
                buf.push(4);
                buf.extend_from_slice(&f.to_le_bytes());
            }
            Value::Text(s) => {
                buf.push(5);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Blob(bytes) => {
                buf.push(6);
                write_varu32(buf, bytes.len() as u32);
                buf.extend_from_slice(bytes);
            }
            Value::Timestamp(t) => {
                buf.push(7);
                buf.extend_from_slice(&t.to_le_bytes());
            }
            Value::Duration(d) => {
                buf.push(8);
                buf.extend_from_slice(&d.to_le_bytes());
            }
            Value::IpAddr(ip) => {
                buf.push(9);
                match ip {
                    IpAddr::V4(v4) => {
                        buf.push(4);
                        buf.extend_from_slice(&v4.octets());
                    }
                    IpAddr::V6(v6) => {
                        buf.push(6);
                        buf.extend_from_slice(&v6.octets());
                    }
                }
            }
            Value::MacAddr(mac) => {
                buf.push(10);
                buf.extend_from_slice(mac);
            }
            Value::Vector(vec) => {
                buf.push(11);
                write_varu32(buf, vec.len() as u32);
                for f in vec {
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
            Value::Json(bytes) => {
                buf.push(12);
                write_varu32(buf, bytes.len() as u32);
                buf.extend_from_slice(bytes);
            }
            Value::Uuid(uuid) => {
                buf.push(13);
                buf.extend_from_slice(uuid);
            }
            Value::NodeRef(s) => {
                buf.push(14);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::EdgeRef(s) => {
                buf.push(15);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::VectorRef(s, id) => {
                buf.push(16);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(&id.to_le_bytes());
            }
            Value::RowRef(s, id) => {
                buf.push(17);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(&id.to_le_bytes());
            }
            Value::Color(rgb) => {
                buf.push(18);
                buf.extend_from_slice(rgb);
            }
            Value::Email(s) => {
                buf.push(19);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Url(s) => {
                buf.push(20);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Phone(n) => {
                buf.push(21);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Semver(v) => {
                buf.push(22);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Cidr(ip, prefix) => {
                buf.push(23);
                buf.extend_from_slice(&ip.to_le_bytes());
                buf.push(*prefix);
            }
            Value::Date(d) => {
                buf.push(24);
                buf.extend_from_slice(&d.to_le_bytes());
            }
            Value::Time(t) => {
                buf.push(25);
                buf.extend_from_slice(&t.to_le_bytes());
            }
            Value::Decimal(v) => {
                buf.push(26);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::EnumValue(i) => {
                buf.push(27);
                buf.push(*i);
            }
            Value::Array(elems) => {
                buf.push(28);
                write_varu32(buf, elems.len() as u32);
                for elem in elems {
                    Self::write_value_binary(buf, elem);
                }
            }
            Value::TimestampMs(v) => {
                buf.push(29);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Ipv4(v) => {
                buf.push(30);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Ipv6(bytes) => {
                buf.push(31);
                buf.extend_from_slice(bytes);
            }
            Value::Subnet(ip, mask) => {
                buf.push(32);
                buf.extend_from_slice(&ip.to_le_bytes());
                buf.extend_from_slice(&mask.to_le_bytes());
            }
            Value::Port(v) => {
                buf.push(33);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Latitude(v) => {
                buf.push(34);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Longitude(v) => {
                buf.push(35);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::GeoPoint(lat, lon) => {
                buf.push(36);
                buf.extend_from_slice(&lat.to_le_bytes());
                buf.extend_from_slice(&lon.to_le_bytes());
            }
            Value::Country2(c) => {
                buf.push(37);
                buf.extend_from_slice(c);
            }
            Value::Country3(c) => {
                buf.push(38);
                buf.extend_from_slice(c);
            }
            Value::Lang2(c) => {
                buf.push(39);
                buf.extend_from_slice(c);
            }
            Value::Lang5(c) => {
                buf.push(40);
                buf.extend_from_slice(c);
            }
            Value::Currency(c) => {
                buf.push(41);
                buf.extend_from_slice(c);
            }
            Value::ColorAlpha(rgba) => {
                buf.push(42);
                buf.extend_from_slice(rgba);
            }
            Value::BigInt(v) => {
                buf.push(43);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::KeyRef(col, key) => {
                buf.push(44);
                write_varu32(buf, col.len() as u32);
                buf.extend_from_slice(col.as_bytes());
                write_varu32(buf, key.len() as u32);
                buf.extend_from_slice(key.as_bytes());
            }
            Value::DocRef(col, id) => {
                buf.push(45);
                write_varu32(buf, col.len() as u32);
                buf.extend_from_slice(col.as_bytes());
                buf.extend_from_slice(&id.to_le_bytes());
            }
            Value::TableRef(name) => {
                buf.push(46);
                write_varu32(buf, name.len() as u32);
                buf.extend_from_slice(name.as_bytes());
            }
            Value::PageRef(page_id) => {
                buf.push(47);
                buf.extend_from_slice(&page_id.to_le_bytes());
            }
        }
    }
}
