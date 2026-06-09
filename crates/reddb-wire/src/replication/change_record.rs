use serde_json::Value as JsonValue;

use super::util::{hex_decode, hex_encode};

pub const DEFAULT_REPLICATION_TERM: u64 = 1;
pub type ChangeRecordJsonValue = JsonValue;

pub fn parse_change_record_json_value(text: &str) -> Result<ChangeRecordJsonValue, String> {
    serde_json::from_str(text).map_err(|err| err.to_string())
}

pub fn change_record_json_value_to_string(value: &ChangeRecordJsonValue) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOperation {
    Insert,
    Update,
    Delete,
    Refresh,
}

impl ChangeOperation {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::from_wire_str(value)
    }

    pub fn from_wire_str(value: &str) -> Option<Self> {
        match value {
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            "refresh" => Some(Self::Refresh),
            _ => None,
        }
    }

    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Refresh => "refresh",
        }
    }

    pub fn as_str(&self) -> &'static str {
        self.as_wire_str()
    }
}

#[derive(Debug, Clone)]
pub struct ChangeRecord {
    pub term: u64,
    pub lsn: u64,
    pub timestamp: u64,
    pub operation: ChangeOperation,
    pub collection: String,
    pub entity_id: u64,
    pub entity_kind: String,
    pub entity_bytes: Option<Vec<u8>>,
    pub metadata: Option<JsonValue>,
    pub refresh_records: Option<Vec<Vec<u8>>>,
}

impl ChangeRecord {
    pub fn for_refresh(
        lsn: u64,
        timestamp: u64,
        collection: impl Into<String>,
        records: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            term: DEFAULT_REPLICATION_TERM,
            lsn,
            timestamp,
            operation: ChangeOperation::Refresh,
            collection: collection.into(),
            entity_id: 0,
            entity_kind: "refresh".to_string(),
            entity_bytes: None,
            metadata: None,
            refresh_records: Some(records),
        }
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut object = serde_json::Map::new();
        object.insert("term".to_string(), JsonValue::Number(self.term.into()));
        object.insert("lsn".to_string(), JsonValue::Number(self.lsn.into()));
        object.insert(
            "timestamp".to_string(),
            JsonValue::Number(self.timestamp.into()),
        );
        object.insert(
            "operation".to_string(),
            JsonValue::String(self.operation.as_wire_str().to_string()),
        );
        object.insert(
            "collection".to_string(),
            JsonValue::String(self.collection.clone()),
        );
        object.insert("rid".to_string(), JsonValue::Number(self.entity_id.into()));
        object.insert(
            "kind".to_string(),
            JsonValue::String(public_item_kind(&self.entity_kind).to_string()),
        );
        if let Some(bytes) = &self.entity_bytes {
            object.insert(
                "entity_bytes_hex".to_string(),
                JsonValue::String(hex_encode(bytes)),
            );
        }
        if let Some(metadata) = &self.metadata {
            object.insert("metadata".to_string(), metadata.clone());
        }
        if let Some(records) = &self.refresh_records {
            let arr = records
                .iter()
                .map(|bytes| JsonValue::String(hex_encode(bytes)))
                .collect();
            object.insert("refresh_records_hex".to_string(), JsonValue::Array(arr));
        }
        JsonValue::Object(object)
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_string(&self.to_json_value())
            .unwrap_or_else(|_| "{}".to_string())
            .into_bytes()
    }

    pub fn with_term(mut self, term: u64) -> Self {
        self.term = term;
        self
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|err| err.to_string())?;
        let value = serde_json::from_str::<JsonValue>(text).map_err(|err| err.to_string())?;
        let operation = value
            .get("operation")
            .and_then(JsonValue::as_str)
            .and_then(ChangeOperation::from_wire_str)
            .ok_or_else(|| "invalid replication operation".to_string())?;
        let entity_bytes = value
            .get("entity_bytes_hex")
            .and_then(JsonValue::as_str)
            .map(|value| hex_decode_string("entity_bytes_hex", value))
            .transpose()?;

        Ok(Self {
            term: value
                .get("term")
                .and_then(JsonValue::as_u64)
                .unwrap_or(DEFAULT_REPLICATION_TERM),
            lsn: value.get("lsn").and_then(JsonValue::as_u64).unwrap_or(0),
            timestamp: value
                .get("timestamp")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            operation,
            collection: value
                .get("collection")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            entity_id: value
                .get("rid")
                .or_else(|| value.get("entity_id"))
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            entity_kind: value
                .get("kind")
                .or_else(|| value.get("entity_kind"))
                .and_then(JsonValue::as_str)
                .unwrap_or("entity")
                .to_string(),
            entity_bytes,
            metadata: value.get("metadata").cloned(),
            refresh_records: match value.get("refresh_records_hex") {
                Some(JsonValue::Array(items)) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        let hex_str = item
                            .as_str()
                            .ok_or_else(|| "refresh_records_hex entry not a string".to_string())?;
                        out.push(hex_decode_string("refresh_records_hex", hex_str)?);
                    }
                    Some(out)
                }
                None | Some(JsonValue::Null) => None,
                _ => return Err("refresh_records_hex is not an array".to_string()),
            },
        })
    }
}

pub fn public_item_kind(entity_kind: &str) -> &'static str {
    match entity_kind {
        "table" | "entity" | "row" => "row",
        "graph_node" | "node" => "node",
        "graph_edge" | "edge" => "edge",
        "kv" => "kv",
        "document" => "document",
        "vector" => "vector",
        other if other.contains("kv") => "kv",
        other if other.contains("document") => "document",
        other if other.contains("vector") => "vector",
        _ => "item",
    }
}

fn hex_decode_string(field: &'static str, value: &str) -> Result<Vec<u8>, String> {
    hex_decode(field, value).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_record_round_trips_json_wire_payload() {
        let record = ChangeRecord {
            term: 3,
            lsn: 7,
            timestamp: 1234,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 42,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: Some(serde_json::json!({"role": "admin"})),
            refresh_records: None,
        };

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");

        assert_eq!(decoded.term, record.term);
        assert_eq!(decoded.lsn, record.lsn);
        assert_eq!(decoded.collection, record.collection);
        assert_eq!(decoded.entity_id, record.entity_id);
        assert_eq!(decoded.entity_bytes, record.entity_bytes);
        assert_eq!(decoded.metadata, record.metadata);
    }

    #[test]
    fn refresh_records_round_trip_without_reordering() {
        let records = vec![vec![0x10, 0x20, 0x30], vec![0xAA, 0xBB], Vec::new()];
        let record =
            ChangeRecord::for_refresh(11, 99, "mv_orders_summary", records.clone()).with_term(4);

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");

        assert_eq!(decoded.term, 4);
        assert_eq!(decoded.operation, ChangeOperation::Refresh);
        assert_eq!(decoded.collection, "mv_orders_summary");
        assert_eq!(decoded.refresh_records.as_deref(), Some(&records[..]));
    }

    #[test]
    fn legacy_change_record_defaults_term() {
        let legacy =
            br#"{"lsn":9,"timestamp":1,"operation":"delete","collection":"users","rid":5,"kind":"row"}"#;

        let decoded = ChangeRecord::decode(legacy).expect("decode legacy record");

        assert_eq!(decoded.term, DEFAULT_REPLICATION_TERM);
        assert_eq!(decoded.lsn, 9);
    }
}
