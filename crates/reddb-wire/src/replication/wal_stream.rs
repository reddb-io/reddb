use serde_json::Value as JsonValue;

use super::util::{
    get_bool_default, get_opt_string, get_opt_u64, get_string, get_u64, hex_decode, hex_encode,
    object_from_slice, ReplicationPayloadError, Result,
};
use super::{catchup::CatchupModeReply, DEFAULT_REPLICATION_TERM};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalStreamOpen {
    pub since_lsn: u64,
    pub max_count: usize,
    pub replica_id: Option<String>,
    pub installed_snapshot_token: Option<String>,
    pub term: u64,
    pub await_data: bool,
    pub await_timeout_ms: u64,
}

impl WalStreamOpen {
    pub fn encode_json(&self) -> Vec<u8> {
        self.encode_json_with_endpoints(None)
    }

    pub fn encode_json_with_endpoints(
        &self,
        endpoints: Option<&crate::routing::RoutingEndpoints>,
    ) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "since_lsn".to_string(),
            JsonValue::Number(self.since_lsn.into()),
        );
        obj.insert(
            "max_count".to_string(),
            JsonValue::Number((self.max_count as u64).into()),
        );
        if let Some(replica_id) = &self.replica_id {
            obj.insert(
                "replica_id".to_string(),
                JsonValue::String(replica_id.clone()),
            );
        }
        if let Some(token) = &self.installed_snapshot_token {
            obj.insert(
                "installed_snapshot_token".to_string(),
                JsonValue::String(token.clone()),
            );
        }
        obj.insert("term".to_string(), JsonValue::Number(self.term.into()));
        obj.insert("await_data".to_string(), JsonValue::Bool(self.await_data));
        obj.insert(
            "await_timeout_ms".to_string(),
            JsonValue::Number(self.await_timeout_ms.into()),
        );
        if let Some(endpoints) = endpoints {
            obj.insert("endpoints".to_string(), serde_json::json!(endpoints));
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        Self::decode_json_with_endpoints(bytes).map(|(open, _)| open)
    }

    pub fn decode_json_with_endpoints(
        bytes: &[u8],
    ) -> Result<(Self, Option<crate::routing::RoutingEndpoints>)> {
        let obj = object_from_slice(bytes)?;
        let endpoints = obj.get("endpoints").map(crate::routing::routing_endpoints);
        let max_count = get_opt_u64(&obj, "max_count").unwrap_or(1000);
        let open = Self {
            since_lsn: get_opt_u64(&obj, "since_lsn").unwrap_or(0),
            max_count: usize::try_from(max_count)
                .map_err(|_| ReplicationPayloadError::InvalidField("max_count"))?,
            replica_id: get_opt_string(&obj, "replica_id"),
            installed_snapshot_token: get_opt_string(&obj, "installed_snapshot_token"),
            term: get_opt_u64(&obj, "term").unwrap_or(DEFAULT_REPLICATION_TERM),
            await_data: get_bool_default(&obj, "await_data", false),
            await_timeout_ms: get_opt_u64(&obj, "await_timeout_ms").unwrap_or(30_000),
        };
        Ok((open, endpoints))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalStreamRecord {
    pub lsn: u64,
    pub data: Vec<u8>,
}

impl WalStreamRecord {
    fn to_json(&self) -> JsonValue {
        let mut obj = serde_json::Map::new();
        obj.insert("lsn".to_string(), JsonValue::Number(self.lsn.into()));
        obj.insert(
            "data".to_string(),
            JsonValue::String(hex_encode(&self.data)),
        );
        JsonValue::Object(obj)
    }

    fn from_json(value: &JsonValue) -> Result<Self> {
        let obj = value
            .as_object()
            .ok_or(ReplicationPayloadError::InvalidField("records"))?;
        let data_hex = get_string(obj, "data")?;
        Ok(Self {
            lsn: get_u64(obj, "lsn")?,
            data: hex_decode("data", &data_hex)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalStreamChunk {
    pub records: Vec<WalStreamRecord>,
    pub current_lsn: u64,
    pub oldest_available_lsn: Option<u64>,
    pub partial_resync: bool,
    pub partial_resync_count: u64,
    pub needs_rebootstrap: bool,
    pub invalidation_reason: Option<String>,
    pub catchup: Option<CatchupModeReply>,
}

impl WalStreamChunk {
    pub fn encode_json(&self) -> Vec<u8> {
        self.encode_json_with_endpoints(None)
    }

    pub fn encode_json_with_endpoints(
        &self,
        endpoints: Option<&crate::routing::RoutingEndpoints>,
    ) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "records".to_string(),
            JsonValue::Array(self.records.iter().map(WalStreamRecord::to_json).collect()),
        );
        obj.insert(
            "current_lsn".to_string(),
            JsonValue::Number(self.current_lsn.into()),
        );
        if let Some(lsn) = self.oldest_available_lsn {
            obj.insert(
                "oldest_available_lsn".to_string(),
                JsonValue::Number(lsn.into()),
            );
        }
        obj.insert(
            "partial_resync".to_string(),
            JsonValue::Bool(self.partial_resync),
        );
        obj.insert(
            "partial_resync_count".to_string(),
            JsonValue::Number(self.partial_resync_count.into()),
        );
        obj.insert(
            "needs_rebootstrap".to_string(),
            JsonValue::Bool(self.needs_rebootstrap),
        );
        if let Some(reason) = &self.invalidation_reason {
            obj.insert(
                "invalidation_reason".to_string(),
                JsonValue::String(reason.clone()),
            );
        }
        if let Some(catchup) = &self.catchup {
            let catchup_obj = object_from_slice(&catchup.encode_json())
                .expect("CatchupModeReply emits a JSON object");
            for (key, value) in catchup_obj {
                obj.insert(key, value);
            }
        }
        if let Some(endpoints) = endpoints {
            obj.insert(
                "primary_endpoints".to_string(),
                serde_json::json!(endpoints),
            );
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        Self::decode_json_with_endpoints(bytes).map(|(chunk, _)| chunk)
    }

    pub fn decode_json_with_endpoints(
        bytes: &[u8],
    ) -> Result<(Self, Option<crate::routing::RoutingEndpoints>)> {
        let obj = object_from_slice(bytes)?;
        let endpoints = obj
            .get("primary_endpoints")
            .map(crate::routing::routing_endpoints);
        let records = match obj.get("records") {
            Some(JsonValue::Array(values)) => values
                .iter()
                .map(WalStreamRecord::from_json)
                .collect::<Result<Vec<_>>>()?,
            Some(_) => return Err(ReplicationPayloadError::InvalidField("records")),
            None => Vec::new(),
        };
        let chunk = Self {
            records,
            current_lsn: get_u64(&obj, "current_lsn")?,
            oldest_available_lsn: get_opt_u64(&obj, "oldest_available_lsn"),
            partial_resync: get_bool_default(&obj, "partial_resync", false),
            partial_resync_count: get_opt_u64(&obj, "partial_resync_count").unwrap_or(0),
            needs_rebootstrap: get_bool_default(&obj, "needs_rebootstrap", false),
            invalidation_reason: get_opt_string(&obj, "invalidation_reason"),
            catchup: CatchupModeReply::from_wal_rebootstrap_object(&obj)?,
        };
        Ok((chunk, endpoints))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalStreamAck {
    pub replica_id: String,
    pub applied_lsn: u64,
    pub durable_lsn: u64,
    pub apply_errors_total: u64,
    pub divergence_total: u64,
}

impl WalStreamAck {
    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "replica_id".to_string(),
            JsonValue::String(self.replica_id.clone()),
        );
        obj.insert(
            "applied_lsn".to_string(),
            JsonValue::Number(self.applied_lsn.into()),
        );
        obj.insert(
            "durable_lsn".to_string(),
            JsonValue::Number(self.durable_lsn.into()),
        );
        obj.insert(
            "apply_errors_total".to_string(),
            JsonValue::Number(self.apply_errors_total.into()),
        );
        obj.insert(
            "divergence_total".to_string(),
            JsonValue::Number(self.divergence_total.into()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        let applied_lsn = get_u64(&obj, "applied_lsn")?;
        Ok(Self {
            replica_id: get_string(&obj, "replica_id")?,
            applied_lsn,
            durable_lsn: get_opt_u64(&obj, "durable_lsn").unwrap_or(applied_lsn),
            apply_errors_total: get_opt_u64(&obj, "apply_errors_total").unwrap_or(0),
            divergence_total: get_opt_u64(&obj, "divergence_total").unwrap_or(0),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalStreamAckReply {
    pub ok: bool,
    pub replica_id: String,
    pub applied_lsn: u64,
    pub durable_lsn: u64,
    pub apply_errors_total: u64,
    pub divergence_total: u64,
}

impl WalStreamAckReply {
    pub fn from_ack(ack: &WalStreamAck) -> Self {
        Self {
            ok: true,
            replica_id: ack.replica_id.clone(),
            applied_lsn: ack.applied_lsn,
            durable_lsn: ack.durable_lsn,
            apply_errors_total: ack.apply_errors_total,
            divergence_total: ack.divergence_total,
        }
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(self.ok));
        obj.insert(
            "replica_id".to_string(),
            JsonValue::String(self.replica_id.clone()),
        );
        obj.insert(
            "applied_lsn".to_string(),
            JsonValue::Number(self.applied_lsn.into()),
        );
        obj.insert(
            "durable_lsn".to_string(),
            JsonValue::Number(self.durable_lsn.into()),
        );
        obj.insert(
            "apply_errors_total".to_string(),
            JsonValue::Number(self.apply_errors_total.into()),
        );
        obj.insert(
            "divergence_total".to_string(),
            JsonValue::Number(self.divergence_total.into()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            ok: get_bool_default(&obj, "ok", false),
            replica_id: get_string(&obj, "replica_id")?,
            applied_lsn: get_u64(&obj, "applied_lsn")?,
            durable_lsn: get_u64(&obj, "durable_lsn")?,
            apply_errors_total: get_opt_u64(&obj, "apply_errors_total").unwrap_or(0),
            divergence_total: get_opt_u64(&obj, "divergence_total").unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::CatchupMode;

    #[test]
    fn wal_stream_open_round_trips() {
        let open = WalStreamOpen {
            since_lsn: 10,
            max_count: 128,
            replica_id: Some("replica-a".to_string()),
            installed_snapshot_token: Some("snapshot-proof".to_string()),
            term: 7,
            await_data: true,
            await_timeout_ms: 5000,
        };
        assert_eq!(
            WalStreamOpen::decode_json(&open.encode_json()).unwrap(),
            open
        );
        let endpoints = crate::routing::RoutingEndpoints::from([(
            "https".to_string(),
            "https://replica:8443".to_string(),
        )]);
        let advertised = open.encode_json_with_endpoints(Some(&endpoints));
        assert_eq!(
            WalStreamOpen::decode_json(&advertised).expect("legacy fields"),
            open
        );
        assert_eq!(
            WalStreamOpen::decode_json_with_endpoints(&advertised).expect("endpoint metadata"),
            (open.clone(), Some(endpoints))
        );
        assert_eq!(
            WalStreamOpen::decode_json_with_endpoints(&open.encode_json())
                .expect("old peer")
                .1,
            None
        );
    }

    #[test]
    fn wal_stream_open_defaults_missing_term_to_legacy_term() {
        let open = WalStreamOpen::decode_json(
            br#"{"since_lsn":10,"max_count":128,"replica_id":"replica-a"}"#,
        )
        .unwrap();
        assert_eq!(open.term, DEFAULT_REPLICATION_TERM);
        assert_eq!(open.installed_snapshot_token, None);
    }

    #[test]
    fn wal_stream_chunk_round_trips_records_and_rebootstrap_hint() {
        let chunk = WalStreamChunk {
            records: vec![WalStreamRecord {
                lsn: 11,
                data: b"record".to_vec(),
            }],
            current_lsn: 12,
            oldest_available_lsn: Some(9),
            partial_resync: true,
            partial_resync_count: 3,
            needs_rebootstrap: true,
            invalidation_reason: Some("retention".to_string()),
            catchup: Some(CatchupModeReply {
                mode: CatchupMode::BaseBackupThenWal,
                available_from_lsn: Some(9),
                replica_lsn: None,
                reason: Some("retention".to_string()),
            }),
        };
        assert_eq!(
            WalStreamChunk::decode_json(&chunk.encode_json()).unwrap(),
            chunk
        );
        let endpoints = crate::routing::RoutingEndpoints::from([(
            "grpc".to_string(),
            "grpc://primary:55055".to_string(),
        )]);
        let advertised = chunk.encode_json_with_endpoints(Some(&endpoints));
        assert_eq!(
            WalStreamChunk::decode_json(&advertised).expect("legacy fields"),
            chunk
        );
        assert_eq!(
            WalStreamChunk::decode_json_with_endpoints(&advertised).expect("endpoint metadata"),
            (chunk.clone(), Some(endpoints))
        );
        assert_eq!(
            WalStreamChunk::decode_json_with_endpoints(&chunk.encode_json())
                .expect("old peer")
                .1,
            None
        );
    }

    #[test]
    fn wal_ack_defaults_durable_to_applied() {
        let ack = WalStreamAck::decode_json(br#"{"replica_id":"r","applied_lsn":7}"#).unwrap();
        assert_eq!(ack.durable_lsn, 7);
        assert_eq!(ack.apply_errors_total, 0);
    }

    #[test]
    fn wal_ack_reply_round_trips() {
        let ack = WalStreamAck {
            replica_id: "replica-a".to_string(),
            applied_lsn: 11,
            durable_lsn: 10,
            apply_errors_total: 2,
            divergence_total: 1,
        };
        let reply = WalStreamAckReply::from_ack(&ack);

        assert_eq!(
            WalStreamAckReply::decode_json(&reply.encode_json()).unwrap(),
            reply
        );
    }
}
