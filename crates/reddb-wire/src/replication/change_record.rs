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
    /// Issue #991 — stable identity of the range this user-data change
    /// belongs to (`RangeId`, from the #989 ownership catalog), or `None`
    /// for records that predate range replication. Carried so replicas and
    /// recovery can route a change to its range and gate it against that
    /// range's authority watermark.
    pub range_id: Option<u64>,
    /// Issue #991 — the owning range's `OwnershipEpoch` at the moment this
    /// change was produced. The epoch bumps only when write authority moves
    /// to a new owner, so a record whose epoch is behind the target range's
    /// accepted epoch is a write from a deposed owner and is fenced. `None`
    /// for legacy records and non-range-replicated changes.
    pub ownership_epoch: Option<u64>,
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
            range_id: None,
            ownership_epoch: None,
        }
    }

    /// Stamp this record with the authority metadata of the range that owns
    /// it (issue #991): the stable range identity and the owner's current
    /// ownership epoch. The term is set independently via [`Self::with_term`].
    pub fn with_range_authority(mut self, range_id: u64, ownership_epoch: u64) -> Self {
        self.range_id = Some(range_id);
        self.ownership_epoch = Some(ownership_epoch);
        self
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
        // Issue #991 — range authority is omitted entirely when absent so a
        // non-range-replicated record stays byte-for-byte the legacy shape.
        if let Some(range_id) = self.range_id {
            object.insert("range_id".to_string(), JsonValue::Number(range_id.into()));
        }
        if let Some(epoch) = self.ownership_epoch {
            object.insert(
                "ownership_epoch".to_string(),
                JsonValue::Number(epoch.into()),
            );
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
            // Issue #991 — absent on legacy records (decodes to `None`), so
            // old payloads keep round-tripping unchanged.
            range_id: value.get("range_id").and_then(JsonValue::as_u64),
            ownership_epoch: value.get("ownership_epoch").and_then(JsonValue::as_u64),
        })
    }
}

/// Issue #991 — the range-authority watermark a replica or recovery target
/// holds for a single range: the minimum term and ownership epoch it will
/// accept for that range's user-data changes.
///
/// The physical WAL stays the source of truth; this fence operates purely on
/// the derived metadata each [`ChangeRecord`] carries. A record stamped for
/// this range whose term or ownership epoch is behind the watermark is a write
/// from a stale (deposed) owner or a superseded timeline and must be rejected
/// before it is applied. Records for a *different* range, or records with no
/// range identity at all (legacy / non-range-replicated), are not this fence's
/// concern and pass through untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeAuthority {
    pub range_id: u64,
    pub min_term: u64,
    pub min_ownership_epoch: u64,
}

/// Why a [`RangeAuthority`] rejected a record (issue #991).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeAdmitError {
    /// The record's term is behind the term this range has accepted — a
    /// returning ex-owner streaming on a superseded timeline.
    StaleTerm {
        record_term: u64,
        accepted_term: u64,
    },
    /// The record's ownership epoch is behind the range's accepted epoch — a
    /// write produced by an owner that has since lost write authority.
    StaleOwnershipEpoch {
        record_epoch: u64,
        accepted_epoch: u64,
    },
}

impl RangeAuthority {
    /// Decide whether `record` may be applied to this range. Only records
    /// explicitly stamped for `self.range_id` are gated; everything else is
    /// admitted. Term is checked before epoch so a stale timeline is reported
    /// as such even when its epoch also lags.
    pub fn admit(&self, record: &ChangeRecord) -> Result<(), RangeAdmitError> {
        if record.range_id != Some(self.range_id) {
            return Ok(());
        }
        if record.term < self.min_term {
            return Err(RangeAdmitError::StaleTerm {
                record_term: record.term,
                accepted_term: self.min_term,
            });
        }
        // A record stamped for this range but missing an epoch predates epoch
        // fencing; admit it on term alone rather than fail closed on absence.
        if let Some(epoch) = record.ownership_epoch {
            if epoch < self.min_ownership_epoch {
                return Err(RangeAdmitError::StaleOwnershipEpoch {
                    record_epoch: epoch,
                    accepted_epoch: self.min_ownership_epoch,
                });
            }
        }
        Ok(())
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
            range_id: None,
            ownership_epoch: None,
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
    fn range_authority_round_trips_on_the_json_wire() {
        let record = ChangeRecord {
            term: 5,
            lsn: 12,
            timestamp: 999,
            operation: ChangeOperation::Insert,
            collection: "orders".to_string(),
            entity_id: 8,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![9, 9]),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        }
        .with_range_authority(7, 3);

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");

        assert_eq!(decoded.range_id, Some(7));
        assert_eq!(decoded.ownership_epoch, Some(3));
        assert_eq!(decoded.term, 5);
    }

    #[test]
    fn legacy_record_without_range_authority_decodes_to_none() {
        // A payload from before #991 carries neither field; decoding must
        // leave both absent rather than fabricate a default that would later
        // collide with a real range fence.
        let legacy =
            br#"{"term":2,"lsn":4,"timestamp":1,"operation":"insert","collection":"users","rid":1,"kind":"row"}"#;

        let decoded = ChangeRecord::decode(legacy).expect("decode legacy");

        assert_eq!(decoded.range_id, None);
        assert_eq!(decoded.ownership_epoch, None);
    }

    #[test]
    fn unstamped_record_omits_range_keys_from_the_wire() {
        let record = ChangeRecord::for_refresh(1, 1, "mv", Vec::new());
        let text = String::from_utf8(record.encode()).expect("utf8");
        assert!(!text.contains("range_id"), "got {text}");
        assert!(!text.contains("ownership_epoch"), "got {text}");
    }

    fn stamped(term: u64, range_id: u64, epoch: u64) -> ChangeRecord {
        ChangeRecord {
            term,
            lsn: 1,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "c".to_string(),
            entity_id: 1,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1]),
            metadata: None,
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        }
        .with_range_authority(range_id, epoch)
    }

    #[test]
    fn range_authority_admits_current_term_and_epoch() {
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 3,
            min_ownership_epoch: 4,
        };
        assert_eq!(fence.admit(&stamped(3, 7, 4)), Ok(()));
        assert_eq!(fence.admit(&stamped(9, 7, 9)), Ok(()));
    }

    #[test]
    fn range_authority_rejects_stale_epoch_and_term() {
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 3,
            min_ownership_epoch: 4,
        };
        assert_eq!(
            fence.admit(&stamped(3, 7, 2)),
            Err(RangeAdmitError::StaleOwnershipEpoch {
                record_epoch: 2,
                accepted_epoch: 4,
            })
        );
        // Term is checked first, so a doubly-stale record reports the term.
        assert_eq!(
            fence.admit(&stamped(1, 7, 1)),
            Err(RangeAdmitError::StaleTerm {
                record_term: 1,
                accepted_term: 3,
            })
        );
    }

    #[test]
    fn range_authority_ignores_other_ranges_and_unstamped_records() {
        let fence = RangeAuthority {
            range_id: 7,
            min_term: 5,
            min_ownership_epoch: 5,
        };
        // Stale, but for a different range — not this fence's business.
        assert_eq!(fence.admit(&stamped(1, 99, 1)), Ok(()));
        // No range identity at all — legacy record passes through.
        let mut legacy = stamped(1, 7, 1);
        legacy.range_id = None;
        legacy.ownership_epoch = None;
        assert_eq!(fence.admit(&legacy), Ok(()));
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
