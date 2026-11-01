//! Retained result capacity, including spare slots and nested model payloads.
//! Shared values are charged per result owner, independently of Arc counts.
use super::*;
use std::alloc::Layout;
use std::mem::size_of;

impl UnifiedResult {
    /// Conservative retained heap estimate without allocating scratch. This is
    /// admission for a completed result, not accounting for executor workspaces.
    pub fn retained_memory_bytes(&self) -> usize {
        let mut bytes = vector_slots(&self.columns)
            .saturating_add(
                self.columns
                    .iter()
                    .map(String::capacity)
                    .fold(0, usize::saturating_add),
            )
            .saturating_add(vector_slots(&self.records))
            .saturating_add(
                self.pre_serialized_json
                    .as_ref()
                    .map_or(0, String::capacity),
            );
        let first_schema = self.records.first().map(|record| &record.schema);
        for (index, record) in self.records.iter().enumerate() {
            bytes = bytes.saturating_add(record_heap_bytes(record));
            // The common shared schema is counted once, with no HashSet or
            // per-row scratch. Other schemas are conservatively charged per row.
            if index == 0 || first_schema.is_none_or(|schema| !Arc::ptr_eq(schema, &record.schema))
            {
                bytes = bytes.saturating_add(schema_bytes(&record.schema));
            }
        }
        if let Some(stats) = &self.stats.vector {
            bytes = bytes
                .saturating_add(vector_slots(&stats.operators))
                .saturating_add(
                    stats
                        .operators
                        .iter()
                        .map(|operator| operator.operator.capacity())
                        .fold(0, usize::saturating_add),
                )
                .saturating_add(stats.mode_requested.capacity())
                .saturating_add(stats.mode_executed.capacity())
                .saturating_add(stats.access_path.capacity())
                .saturating_add(stats.fallback_reason.as_ref().map_or(0, String::capacity));
        }
        bytes
    }
}

fn vector_slots<T>(values: &Vec<T>) -> usize {
    values.capacity().saturating_mul(size_of::<T>())
}

fn arc_text_bytes(value: &Arc<str>) -> usize {
    Layout::array::<u8>(value.len())
        .map(arc_allocation_bytes)
        .unwrap_or(usize::MAX)
}

fn arc_allocation_bytes(payload: Layout) -> usize {
    // Arc's strong/weak counters precede its payload. Include the trailing
    // alignment: e.g. one UTF-8 byte requests 24 bytes on a 64-bit host, not 17.
    Layout::new::<[usize; 2]>()
        .extend(payload)
        .map(|(layout, _)| layout.pad_to_align().size())
        .unwrap_or(usize::MAX)
}

fn schema_bytes(schema: &Arc<Vec<Arc<str>>>) -> usize {
    arc_allocation_bytes(Layout::new::<Vec<Arc<str>>>())
        .saturating_add(vector_slots(schema))
        .saturating_add(
            schema
                .iter()
                .map(arc_text_bytes)
                .fold(0, usize::saturating_add),
        )
}

fn map_bytes<K, V>(map: &HashMap<K, V>, mut payload: impl FnMut(&K, &V) -> usize) -> usize {
    // Capacity is the usable entry count, not the raw bucket count. Twice the
    // usable slots plus control/alignment space conservatively covers sparse
    // SwissTable allocations, including the smallest nonempty table.
    let slots = if map.capacity() == 0 {
        0
    } else {
        map.capacity()
            .saturating_mul(2)
            .saturating_mul(size_of::<(K, V)>().saturating_add(1))
            .saturating_add(64)
    };
    map.iter().fold(slots, |bytes, (key, value)| {
        bytes.saturating_add(payload(key, value))
    })
}

fn properties_bytes(properties: &HashMap<String, Value>) -> usize {
    map_bytes(properties, |key, value| {
        key.capacity().saturating_add(value_heap_bytes(value))
    })
}

fn edge_heap_bytes(edge: &MatchedEdge) -> usize {
    edge.from
        .capacity()
        .saturating_add(edge.to.capacity())
        .saturating_add(edge.edge_label.capacity())
        .saturating_add(properties_bytes(&edge.properties))
}

fn record_heap_bytes(record: &UnifiedRecord) -> usize {
    let mut bytes = vector_slots(&record.values).saturating_add(
        record
            .values
            .iter()
            .map(value_heap_bytes)
            .fold(0, usize::saturating_add),
    );
    if let Some(overflow) = &record.overflow {
        bytes = bytes.saturating_add(map_bytes(overflow, |key, value| {
            arc_text_bytes(key).saturating_add(value_heap_bytes(value))
        }));
    }
    bytes = bytes
        .saturating_add(map_bytes(&record.nodes, |key, node| {
            key.capacity()
                .saturating_add(node.id.capacity())
                .saturating_add(node.label.capacity())
                .saturating_add(node.node_label.capacity())
                .saturating_add(properties_bytes(&node.properties))
        }))
        .saturating_add(map_bytes(&record.edges, |key, edge| {
            key.capacity().saturating_add(edge_heap_bytes(edge))
        }))
        .saturating_add(vector_slots(&record.paths))
        .saturating_add(vector_slots(&record.vector_results));
    for path in &record.paths {
        bytes = bytes
            .saturating_add(vector_slots(&path.nodes))
            .saturating_add(
                path.nodes
                    .iter()
                    .map(String::capacity)
                    .fold(0, usize::saturating_add),
            )
            .saturating_add(vector_slots(&path.edges))
            .saturating_add(
                path.edges
                    .iter()
                    .map(edge_heap_bytes)
                    .fold(0, usize::saturating_add),
            );
    }
    for vector in &record.vector_results {
        bytes = bytes
            .saturating_add(vector.collection.capacity())
            .saturating_add(vector.vector.as_ref().map_or(0, vector_slots))
            .saturating_add(vector.metadata.as_ref().map_or(0, properties_bytes))
            .saturating_add(vector.linked_node.as_ref().map_or(0, String::capacity))
            .saturating_add(
                vector
                    .linked_row
                    .as_ref()
                    .map_or(0, |(table, _)| table.capacity()),
            );
    }
    bytes
}

fn value_heap_bytes(value: &Value) -> usize {
    // Bounded explicit stack: API-supplied values deeper than the parser's
    // maximum cannot receive a representable retained-memory allowance.
    const LEVELS: usize = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 1;
    let mut stack: [std::slice::Iter<'_, Value>; LEVELS] = std::array::from_fn(|_| [].iter());
    let mut depth = 0;
    let mut current = Some(value);
    let mut bytes = 0usize;
    loop {
        if let Some(value) = current.take() {
            if let Value::Array(values) = value {
                if depth == LEVELS {
                    return usize::MAX;
                }
                bytes = bytes.saturating_add(vector_slots(values));
                stack[depth] = values.iter();
                depth += 1;
            } else {
                bytes = bytes.saturating_add(scalar_heap_bytes(value));
            }
        }
        if depth == 0 {
            return bytes;
        }
        current = stack[depth - 1].next();
        if current.is_none() {
            depth -= 1;
        }
    }
}

fn scalar_heap_bytes(value: &Value) -> usize {
    match value {
        Value::Text(value) => arc_text_bytes(value),
        Value::Blob(value) | Value::Json(value) | Value::Secret(value) => value.capacity(),
        Value::Vector(value) => vector_slots(value),
        Value::NodeRef(value)
        | Value::EdgeRef(value)
        | Value::VectorRef(value, _)
        | Value::RowRef(value, _)
        | Value::DocRef(value, _)
        | Value::TableRef(value)
        | Value::Email(value)
        | Value::Url(value)
        | Value::Password(value)
        | Value::DecimalText(value)
        | Value::AssetCode(value) => value.capacity(),
        Value::KeyRef(collection, key) => collection.capacity().saturating_add(key.capacity()),
        Value::Money { asset_code, .. } => asset_code.capacity(),
        Value::Array(_) => unreachable!("arrays use the bounded stack"),
        Value::Null
        | Value::Integer(_)
        | Value::UnsignedInteger(_)
        | Value::Float(_)
        | Value::Boolean(_)
        | Value::Timestamp(_)
        | Value::Duration(_)
        | Value::IpAddr(_)
        | Value::MacAddr(_)
        | Value::Uuid(_)
        | Value::Color(_)
        | Value::Phone(_)
        | Value::Semver(_)
        | Value::Cidr(_, _)
        | Value::Date(_)
        | Value::Time(_)
        | Value::Decimal(_)
        | Value::EnumValue(_)
        | Value::TimestampMs(_)
        | Value::Ipv4(_)
        | Value::Ipv6(_)
        | Value::Subnet(_, _)
        | Value::Port(_)
        | Value::Latitude(_)
        | Value::Longitude(_)
        | Value::GeoPoint(_, _)
        | Value::Country2(_)
        | Value::Country3(_)
        | Value::Lang2(_)
        | Value::Lang5(_)
        | Value::Currency(_)
        | Value::ColorAlpha(_)
        | Value::BigInt(_)
        | Value::PageRef(_) => 0,
    }
}
