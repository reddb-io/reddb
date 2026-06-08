use serde_json::Value as JsonValue;

use super::util::{
    get_opt_string, get_opt_u64, get_string, object_from_slice, ReplicationPayloadError, Result,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchupMode {
    Wal,
    BaseBackupThenWal,
    Reclone,
}

impl CatchupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wal => "wal-only",
            Self::BaseBackupThenWal => "basebackup-then-wal",
            Self::Reclone => "reclone",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wal" | "wal-only" => Some(Self::Wal),
            "basebackup-then-wal" => Some(Self::BaseBackupThenWal),
            "reclone" => Some(Self::Reclone),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchupModeReply {
    pub mode: CatchupMode,
    pub available_from_lsn: Option<u64>,
    pub replica_lsn: Option<u64>,
    pub reason: Option<String>,
}

impl CatchupModeReply {
    pub fn encode_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "catchup_mode".to_string(),
            JsonValue::String(self.mode.as_str().to_string()),
        );
        if let Some(lsn) = self.available_from_lsn {
            obj.insert(
                "available_from_lsn".to_string(),
                JsonValue::Number(lsn.into()),
            );
        }
        if let Some(lsn) = self.replica_lsn {
            obj.insert("replica_lsn".to_string(), JsonValue::Number(lsn.into()));
        }
        if let Some(reason) = &self.reason {
            obj.insert("reason".to_string(), JsonValue::String(reason.clone()));
        }
        serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
    }

    pub fn decode_json(bytes: &[u8]) -> Result<Self> {
        let obj = object_from_slice(bytes)?;
        let mode = CatchupMode::parse(&get_string(&obj, "catchup_mode")?)
            .ok_or(ReplicationPayloadError::InvalidField("catchup_mode"))?;
        Ok(Self {
            mode,
            available_from_lsn: get_opt_u64(&obj, "available_from_lsn"),
            replica_lsn: get_opt_u64(&obj, "replica_lsn"),
            reason: get_opt_string(&obj, "reason"),
        })
    }

    pub(crate) fn from_wal_rebootstrap_object(
        obj: &serde_json::Map<String, JsonValue>,
    ) -> Result<Option<Self>> {
        let Some(mode) = get_opt_string(obj, "catchup_mode") else {
            return Ok(None);
        };
        let mode = CatchupMode::parse(&mode)
            .ok_or(ReplicationPayloadError::InvalidField("catchup_mode"))?;
        Ok(Some(Self {
            mode,
            available_from_lsn: get_opt_u64(obj, "oldest_available_lsn"),
            replica_lsn: None,
            reason: get_opt_string(obj, "invalidation_reason"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catchup_mode_reply_round_trips() {
        let reply = CatchupModeReply {
            mode: CatchupMode::BaseBackupThenWal,
            available_from_lsn: Some(10),
            replica_lsn: Some(7),
            reason: Some("slot-invalidated".to_string()),
        };
        assert_eq!(
            CatchupModeReply::decode_json(&reply.encode_json()).unwrap(),
            reply
        );
    }

    #[test]
    fn catchup_mode_rejects_unknown_mode() {
        assert_eq!(
            CatchupModeReply::decode_json(br#"{"catchup_mode":"x"}"#).unwrap_err(),
            ReplicationPayloadError::InvalidField("catchup_mode")
        );
    }
}
