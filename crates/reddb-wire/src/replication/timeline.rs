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
}
