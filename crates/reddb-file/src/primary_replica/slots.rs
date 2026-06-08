use super::*;

use serde_json::{Map as JsonMap, Value as JsonValue};

const REPLICATION_SLOT_CATALOG_VERSION_V1: u16 = 1;
const REPLICATION_SLOT_CATALOG_VERSION_V2: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationDurability {
    Async,
    RemoteWrite { quorum: u16 },
    RemoteFlush { quorum: u16 },
    RemoteApply { quorum: u16 },
}

impl ReplicationDurability {
    pub fn required_quorum(self) -> u16 {
        match self {
            Self::Async => 0,
            Self::RemoteWrite { quorum }
            | Self::RemoteFlush { quorum }
            | Self::RemoteApply { quorum } => quorum,
        }
    }

    pub fn is_satisfied(self, commit_lsn: u64, acks: &[ReplicaAck]) -> bool {
        match self {
            Self::Async => true,
            Self::RemoteWrite { quorum } => {
                count_matching_acks(acks, |ack| ack.written_lsn >= commit_lsn) >= quorum
            }
            Self::RemoteFlush { quorum } => {
                count_matching_acks(acks, |ack| ack.flushed_lsn >= commit_lsn) >= quorum
            }
            Self::RemoteApply { quorum } => {
                count_matching_acks(acks, |ack| ack.applied_lsn >= commit_lsn) >= quorum
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaAck {
    pub replica_id: String,
    pub timeline: TimelineId,
    pub received_lsn: u64,
    pub written_lsn: u64,
    pub flushed_lsn: u64,
    pub applied_lsn: u64,
}

impl ReplicaAck {
    pub fn new(replica_id: impl Into<String>, timeline: TimelineId) -> Self {
        Self {
            replica_id: replica_id.into(),
            timeline,
            received_lsn: 0,
            written_lsn: 0,
            flushed_lsn: 0,
            applied_lsn: 0,
        }
    }

    pub fn with_positions(
        replica_id: impl Into<String>,
        timeline: TimelineId,
        received_lsn: u64,
        written_lsn: u64,
        flushed_lsn: u64,
        applied_lsn: u64,
    ) -> RdbFileResult<Self> {
        let ack = Self {
            replica_id: replica_id.into(),
            timeline,
            received_lsn,
            written_lsn,
            flushed_lsn,
            applied_lsn,
        };
        ack.validate()?;
        Ok(ack)
    }

    pub fn retention_floor_lsn(&self) -> u64 {
        self.flushed_lsn.min(self.applied_lsn)
    }

    pub(crate) fn validate(&self) -> RdbFileResult<()> {
        if self.applied_lsn > self.flushed_lsn
            || self.flushed_lsn > self.written_lsn
            || self.written_lsn > self.received_lsn
        {
            return Err(RdbFileError::InvalidOperation(format!(
                "replica {} ack positions are not ordered",
                self.replica_id
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationSlotInvalidationCause {
    WalRemoved,
    Horizon,
    IdleTimeout,
}

impl ReplicationSlotInvalidationCause {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WalRemoved => "wal-removed",
            Self::Horizon => "horizon",
            Self::IdleTimeout => "idle-timeout",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wal-removed" => Some(Self::WalRemoved),
            "horizon" => Some(Self::Horizon),
            "idle-timeout" => Some(Self::IdleTimeout),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationSlot {
    pub replica_id: String,
    pub timeline: TimelineId,
    pub restart_lsn: u64,
    pub confirmed_write_lsn: u64,
    pub confirmed_flush_lsn: u64,
    pub confirmed_apply_lsn: u64,
    pub active: bool,
    pub last_seen_at_unix_ms: u128,
    pub invalidation_reason: Option<ReplicationSlotInvalidationCause>,
    pub invalidated_at_unix_ms: Option<u128>,
}

impl ReplicationSlot {
    pub fn new(replica_id: impl Into<String>, timeline: TimelineId, restart_lsn: u64) -> Self {
        Self {
            replica_id: replica_id.into(),
            timeline,
            restart_lsn,
            confirmed_write_lsn: restart_lsn,
            confirmed_flush_lsn: restart_lsn,
            confirmed_apply_lsn: restart_lsn,
            active: true,
            last_seen_at_unix_ms: 0,
            invalidation_reason: None,
            invalidated_at_unix_ms: None,
        }
    }

    pub fn confirmed_lsn(&self) -> u64 {
        self.confirmed_write_lsn
            .max(self.confirmed_flush_lsn)
            .max(self.confirmed_apply_lsn)
    }

    pub fn mark_invalidated(
        &mut self,
        reason: ReplicationSlotInvalidationCause,
        invalidated_at_unix_ms: u128,
    ) {
        self.active = false;
        self.invalidation_reason = Some(reason);
        self.invalidated_at_unix_ms = Some(invalidated_at_unix_ms);
    }

    pub fn update_ack(&mut self, ack: &ReplicaAck) -> RdbFileResult<()> {
        if self.replica_id != ack.replica_id {
            return Err(RdbFileError::InvalidOperation(format!(
                "ack for replica {} does not match slot {}",
                ack.replica_id, self.replica_id
            )));
        }
        if self.timeline != ack.timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "ack timeline {} does not match slot timeline {}",
                ack.timeline.0, self.timeline.0
            )));
        }
        ack.validate()?;
        self.confirmed_write_lsn = self.confirmed_write_lsn.max(ack.written_lsn);
        self.confirmed_flush_lsn = self.confirmed_flush_lsn.max(ack.flushed_lsn);
        self.confirmed_apply_lsn = self.confirmed_apply_lsn.max(ack.applied_lsn);
        self.restart_lsn = self.restart_lsn.max(self.retention_floor_lsn());
        Ok(())
    }

    pub fn retention_floor_lsn(&self) -> u64 {
        self.confirmed_flush_lsn.min(self.confirmed_apply_lsn)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationSlotCatalog {
    pub timeline: TimelineId,
    pub slots: Vec<ReplicationSlot>,
}

impl ReplicationSlotCatalog {
    pub fn new(timeline: TimelineId) -> Self {
        Self {
            timeline,
            slots: Vec::new(),
        }
    }

    pub fn upsert(&mut self, slot: ReplicationSlot) -> RdbFileResult<()> {
        if slot.timeline != self.timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "slot timeline {} does not match catalog timeline {}",
                slot.timeline.0, self.timeline.0
            )));
        }
        if let Some(existing) = self
            .slots
            .iter_mut()
            .find(|existing| existing.replica_id == slot.replica_id)
        {
            *existing = slot;
        } else {
            self.slots.push(slot);
        }
        self.slots
            .sort_by(|left, right| left.replica_id.cmp(&right.replica_id));
        Ok(())
    }

    pub fn retention_floor_lsn(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter(|slot| slot.active)
            .map(ReplicationSlot::retention_floor_lsn)
            .min()
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode()?)
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn read_legacy_json_from_path(path: impl AsRef<Path>, now_ms: u128) -> RdbFileResult<Self> {
        Self::decode_legacy_json(&fs::read(path)?, now_ms)
    }

    pub fn write_legacy_json_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = self.encode_legacy_json()?;
        let temp_path = crate::layout::legacy_logical_slots_temp_path(path);
        {
            let mut temp = File::create(&temp_path)?;
            temp.write_all(&bytes)?;
            temp.sync_all()?;
        }
        fs::rename(&temp_path, path)?;
        Ok(())
    }

    pub fn encode(&self) -> RdbFileResult<Vec<u8>> {
        let mut out = Vec::new();
        out.extend_from_slice(REPLICATION_SLOT_CATALOG_MAGIC);
        put_u16(&mut out, REPLICATION_SLOT_CATALOG_VERSION_V2);
        put_u64(&mut out, self.timeline.0);
        put_u32(&mut out, self.slots.len() as u32);
        for slot in &self.slots {
            if slot.timeline != self.timeline {
                return Err(RdbFileError::InvalidOperation(format!(
                    "slot timeline {} does not match catalog timeline {}",
                    slot.timeline.0, self.timeline.0
                )));
            }
            put_string(&mut out, &slot.replica_id);
            put_u64(&mut out, slot.restart_lsn);
            put_u64(&mut out, slot.confirmed_write_lsn);
            put_u64(&mut out, slot.confirmed_flush_lsn);
            put_u64(&mut out, slot.confirmed_apply_lsn);
            out.push(u8::from(slot.active));
            put_u128(&mut out, slot.last_seen_at_unix_ms);
            put_optional_string(
                &mut out,
                slot.invalidation_reason
                    .as_ref()
                    .map(ReplicationSlotInvalidationCause::as_str),
            );
            put_optional_u128(&mut out, slot.invalidated_at_unix_ms);
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        Ok(out)
    }

    pub fn encode_legacy_json(&self) -> RdbFileResult<Vec<u8>> {
        let slots_json = self
            .slots
            .iter()
            .map(|slot| {
                let mut object = JsonMap::new();
                object.insert("id".to_string(), JsonValue::String(slot.replica_id.clone()));
                object.insert(
                    "restart_lsn".to_string(),
                    JsonValue::Number(slot.restart_lsn.into()),
                );
                object.insert(
                    "confirmed_lsn".to_string(),
                    JsonValue::Number(slot.confirmed_lsn().into()),
                );
                object.insert(
                    "last_seen_at_unix_ms".to_string(),
                    JsonValue::Number(u128_to_json_u64(slot.last_seen_at_unix_ms).into()),
                );
                if let Some(reason) = slot.invalidation_reason {
                    object.insert(
                        "invalidation_reason".to_string(),
                        JsonValue::String(reason.as_str().to_string()),
                    );
                }
                if let Some(invalidated_at) = slot.invalidated_at_unix_ms {
                    object.insert(
                        "invalidated_at_unix_ms".to_string(),
                        JsonValue::Number(u128_to_json_u64(invalidated_at).into()),
                    );
                }
                JsonValue::Object(object)
            })
            .collect();
        let mut root = JsonMap::new();
        root.insert("slots".to_string(), JsonValue::Array(slots_json));
        serde_json::to_vec_pretty(&JsonValue::Object(root))
            .map_err(|err| RdbFileError::InvalidOperation(format!("encode legacy slots: {err}")))
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "replication slot catalog")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            REPLICATION_SLOT_CATALOG_MAGIC,
            "replication slot catalog",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if !matches!(
            version,
            REPLICATION_SLOT_CATALOG_VERSION_V1 | REPLICATION_SLOT_CATALOG_VERSION_V2
        ) {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported replication slot catalog version {version}"
            )));
        }
        let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
        let count = take_u32(bytes, &mut cursor, payload_end)? as usize;
        let mut catalog = Self::new(timeline);
        for _ in 0..count {
            let replica_id = take_string(bytes, &mut cursor, payload_end)?;
            let restart_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let confirmed_write_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let confirmed_flush_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let confirmed_apply_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let active = take_u8(bytes, &mut cursor, payload_end)? != 0;
            let last_seen_at_unix_ms = if version >= REPLICATION_SLOT_CATALOG_VERSION_V2 {
                take_u128(bytes, &mut cursor, payload_end)?
            } else {
                0
            };
            let invalidation_reason = if version >= REPLICATION_SLOT_CATALOG_VERSION_V2 {
                take_optional_string(bytes, &mut cursor, payload_end)?
                    .and_then(|reason| ReplicationSlotInvalidationCause::parse(&reason))
            } else if active {
                None
            } else {
                Some(ReplicationSlotInvalidationCause::Horizon)
            };
            let invalidated_at_unix_ms = if version >= REPLICATION_SLOT_CATALOG_VERSION_V2 {
                take_optional_u128(bytes, &mut cursor, payload_end)?
            } else {
                None
            };
            if confirmed_apply_lsn > confirmed_flush_lsn
                || confirmed_flush_lsn > confirmed_write_lsn
            {
                return Err(RdbFileError::InvalidOperation(format!(
                    "slot {replica_id} positions are not ordered"
                )));
            }
            catalog.upsert(ReplicationSlot {
                replica_id,
                timeline,
                restart_lsn,
                confirmed_write_lsn,
                confirmed_flush_lsn,
                confirmed_apply_lsn,
                active,
                last_seen_at_unix_ms,
                invalidation_reason,
                invalidated_at_unix_ms,
            })?;
        }
        reject_trailing_bytes(bytes, cursor, payload_end, "replication slot catalog")?;
        Ok(catalog)
    }

    pub fn decode_legacy_json(bytes: &[u8], now_ms: u128) -> RdbFileResult<Self> {
        let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
            RdbFileError::InvalidOperation(format!("decode legacy replication slots: {err}"))
        })?;
        let mut catalog = Self::new(TimelineId::initial());
        let slots = value
            .get("slots")
            .and_then(JsonValue::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for value in slots {
            let Some(object) = value.as_object() else {
                continue;
            };
            let Some(id) = object.get("id").and_then(JsonValue::as_str) else {
                continue;
            };
            let Some(restart_lsn) = object.get("restart_lsn").and_then(JsonValue::as_u64) else {
                continue;
            };
            let Some(confirmed_lsn) = object.get("confirmed_lsn").and_then(JsonValue::as_u64)
            else {
                continue;
            };
            let last_seen_at_unix_ms = object
                .get("last_seen_at_unix_ms")
                .and_then(JsonValue::as_u64)
                .map(u128::from)
                .unwrap_or(now_ms);
            let invalidation_reason = object
                .get("invalidation_reason")
                .and_then(JsonValue::as_str)
                .and_then(ReplicationSlotInvalidationCause::parse);
            let invalidated_at_unix_ms = object
                .get("invalidated_at_unix_ms")
                .and_then(JsonValue::as_u64)
                .map(u128::from);
            let mut slot = ReplicationSlot::new(id.to_string(), TimelineId::initial(), restart_lsn);
            slot.confirmed_write_lsn = confirmed_lsn.max(restart_lsn);
            slot.confirmed_flush_lsn = restart_lsn;
            slot.confirmed_apply_lsn = restart_lsn;
            slot.last_seen_at_unix_ms = last_seen_at_unix_ms;
            slot.active = invalidation_reason.is_none();
            slot.invalidation_reason = invalidation_reason;
            slot.invalidated_at_unix_ms = invalidated_at_unix_ms;
            catalog.upsert(slot)?;
        }
        Ok(catalog)
    }
}

fn count_matching_acks(acks: &[ReplicaAck], predicate: impl Fn(&ReplicaAck) -> bool) -> u16 {
    acks.iter().filter(|ack| predicate(ack)).count() as u16
}

fn u128_to_json_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
