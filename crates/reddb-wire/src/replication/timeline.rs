use serde_json::Value as JsonValue;

use super::util::{get_opt_string, get_opt_u64, get_string, get_u64, object_from_slice, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineForkNotice {
    pub parent_timeline: u64,
    pub new_timeline: u64,
    pub fork_lsn: u64,
    pub promoted_replica_id: Option<String>,
    pub created_at_unix_ms: Option<u64>,
}

impl TimelineForkNotice {
    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "parent_timeline".to_string(),
            JsonValue::Number(self.parent_timeline.into()),
        );
        obj.insert(
            "new_timeline".to_string(),
            JsonValue::Number(self.new_timeline.into()),
        );
        obj.insert(
            "fork_lsn".to_string(),
            JsonValue::Number(self.fork_lsn.into()),
        );
        if let Some(replica_id) = &self.promoted_replica_id {
            obj.insert(
                "promoted_replica_id".to_string(),
                JsonValue::String(replica_id.clone()),
            );
        }
        if let Some(created_at) = self.created_at_unix_ms {
            obj.insert(
                "created_at_unix_ms".to_string(),
                JsonValue::Number(created_at.into()),
            );
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            parent_timeline: get_u64(&obj, "parent_timeline")?,
            new_timeline: get_u64(&obj, "new_timeline")?,
            fork_lsn: get_u64(&obj, "fork_lsn")?,
            promoted_replica_id: get_opt_string(&obj, "promoted_replica_id"),
            created_at_unix_ms: get_opt_u64(&obj, "created_at_unix_ms"),
        })
    }

    pub fn decode_legacy_rejoin_plan(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            parent_timeline: get_opt_u64(&obj, "rejoin_node_timeline").unwrap_or(0),
            new_timeline: get_u64(&obj, "rejoin_target_timeline")?,
            fork_lsn: get_u64(&obj, "rejoin_start_lsn")
                .or_else(|_| get_u64(&obj, "rejoin_rewind_to_lsn"))?,
            promoted_replica_id: get_opt_string(&obj, "promoted_replica_id")
                .or_else(|| get_opt_string(&obj, "replica_id")),
            created_at_unix_ms: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejoinPlanNotice {
    pub state: String,
    pub node_timeline: u64,
    pub node_flushed_lsn: u64,
    pub available_from_lsn: u64,
    pub target_timeline: u64,
    pub rewind_to_lsn: Option<u64>,
    pub start_lsn: u64,
}

impl RejoinPlanNotice {
    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            state: get_string(&obj, "state")?,
            node_timeline: get_u64(&obj, "rejoin_node_timeline")?,
            node_flushed_lsn: get_u64(&obj, "rejoin_node_flushed_lsn")?,
            available_from_lsn: get_u64(&obj, "rejoin_available_from_lsn")?,
            target_timeline: get_u64(&obj, "rejoin_target_timeline")?,
            rewind_to_lsn: get_opt_u64(&obj, "rejoin_rewind_to_lsn"),
            start_lsn: get_u64(&obj, "rejoin_start_lsn")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejoinRewindConfirmation {
    pub target_timeline: u64,
    pub rewind_to_lsn: u64,
}

impl RejoinRewindConfirmation {
    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            target_timeline: get_u64(&obj, "target_timeline")?,
            rewind_to_lsn: get_u64(&obj, "rewind_to_lsn")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejoinRewindConfirmationReply {
    pub ok: bool,
    pub target_timeline: u64,
    pub rewind_to_lsn: u64,
    pub next_step: String,
}

impl RejoinRewindConfirmationReply {
    pub fn confirmed(target_timeline: u64, rewind_to_lsn: u64) -> Self {
        Self {
            ok: true,
            target_timeline,
            rewind_to_lsn,
            next_step: "restart or resume replica apply from the confirmed LSN".to_string(),
        }
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(self.ok));
        obj.insert(
            "target_timeline".to_string(),
            JsonValue::Number(self.target_timeline.into()),
        );
        obj.insert(
            "rewind_to_lsn".to_string(),
            JsonValue::Number(self.rewind_to_lsn.into()),
        );
        obj.insert(
            "next_step".to_string(),
            JsonValue::String(self.next_step.clone()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            ok: obj.get("ok").and_then(JsonValue::as_bool).unwrap_or(false),
            target_timeline: get_u64(&obj, "target_timeline")?,
            rewind_to_lsn: get_u64(&obj, "rewind_to_lsn")?,
            next_step: get_string(&obj, "next_step")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverPromotionRequest {
    pub holder_id: Option<String>,
    pub ttl_ms: Option<u64>,
}

impl FailoverPromotionRequest {
    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            holder_id: get_opt_string(&obj, "holder_id"),
            ttl_ms: get_opt_u64(&obj, "ttl_ms").filter(|ttl| *ttl > 0),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverPromotionReply {
    pub ok: bool,
    pub holder_id: String,
    pub generation: u64,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
    pub timeline: u64,
    pub applied_lsn: u64,
    pub next_step: String,
}

impl FailoverPromotionReply {
    pub fn promoted(
        holder_id: impl Into<String>,
        generation: u64,
        acquired_at_ms: u64,
        expires_at_ms: u64,
        timeline: u64,
        applied_lsn: u64,
    ) -> Self {
        Self {
            ok: true,
            holder_id: holder_id.into(),
            generation,
            acquired_at_ms,
            expires_at_ms,
            timeline,
            applied_lsn,
            next_step: "restart with RED_REPLICATION_MODE=primary to start accepting writes"
                .to_string(),
        }
    }

    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(self.ok));
        obj.insert(
            "holder_id".to_string(),
            JsonValue::String(self.holder_id.clone()),
        );
        obj.insert(
            "generation".to_string(),
            JsonValue::Number(self.generation.into()),
        );
        obj.insert(
            "acquired_at_ms".to_string(),
            JsonValue::Number(self.acquired_at_ms.into()),
        );
        obj.insert(
            "expires_at_ms".to_string(),
            JsonValue::Number(self.expires_at_ms.into()),
        );
        obj.insert(
            "timeline".to_string(),
            JsonValue::Number(self.timeline.into()),
        );
        obj.insert(
            "applied_lsn".to_string(),
            JsonValue::Number(self.applied_lsn.into()),
        );
        obj.insert(
            "next_step".to_string(),
            JsonValue::String(self.next_step.clone()),
        );
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        Ok(Self {
            ok: obj.get("ok").and_then(JsonValue::as_bool).unwrap_or(false),
            holder_id: get_string(&obj, "holder_id")?,
            generation: get_u64(&obj, "generation")?,
            acquired_at_ms: get_u64(&obj, "acquired_at_ms")?,
            expires_at_ms: get_u64(&obj, "expires_at_ms")?,
            timeline: get_u64(&obj, "timeline")?,
            applied_lsn: get_u64(&obj, "applied_lsn")?,
            next_step: get_string(&obj, "next_step")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_fork_notice_round_trips() {
        let notice = TimelineForkNotice {
            parent_timeline: 1,
            new_timeline: 2,
            fork_lsn: 42,
            promoted_replica_id: Some("replica-a".to_string()),
            created_at_unix_ms: Some(1000),
        };
        assert_eq!(
            TimelineForkNotice::decode_json(&notice.encode_json()).unwrap(),
            notice
        );
    }

    #[test]
    fn rejoin_plan_notice_decodes_existing_runtime_shape() {
        let plan = RejoinPlanNotice::decode_json(
            br#"{
                "state":"rejoin_rewind_required",
                "rejoin_node_timeline":1,
                "rejoin_node_flushed_lsn":60,
                "rejoin_available_from_lsn":40,
                "rejoin_target_timeline":3,
                "rejoin_rewind_to_lsn":42,
                "rejoin_start_lsn":42
            }"#,
        )
        .unwrap();
        assert_eq!(plan.target_timeline, 3);
        assert_eq!(plan.rewind_to_lsn, Some(42));
    }

    #[test]
    fn rejoin_rewind_confirmation_contract_round_trips() {
        let request =
            RejoinRewindConfirmation::decode_json(br#"{"target_timeline":3,"rewind_to_lsn":42}"#)
                .unwrap();
        assert_eq!(request.target_timeline, 3);
        assert_eq!(request.rewind_to_lsn, 42);

        let reply = RejoinRewindConfirmationReply::confirmed(3, 42);
        assert_eq!(
            RejoinRewindConfirmationReply::decode_json(&reply.encode_json()).unwrap(),
            reply
        );
    }

    #[test]
    fn failover_promotion_payloads_round_trip() {
        let request =
            FailoverPromotionRequest::decode_json(br#"{"holder_id":"replica-a","ttl_ms":30000}"#)
                .unwrap();
        assert_eq!(request.holder_id.as_deref(), Some("replica-a"));
        assert_eq!(request.ttl_ms, Some(30_000));

        let reply = FailoverPromotionReply::promoted("replica-a", 7, 100, 200, 2, 42);
        assert_eq!(
            FailoverPromotionReply::decode_json(&reply.encode_json()).unwrap(),
            reply
        );
    }
}
