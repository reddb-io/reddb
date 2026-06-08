use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineId(pub u64);

impl TimelineId {
    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineHistoryEntry {
    pub timeline: TimelineId,
    pub parent_timeline: Option<TimelineId>,
    pub fork_lsn: u64,
    pub created_at_unix_ms: u64,
    pub reason: String,
}

impl TimelineHistoryEntry {
    pub fn initial(created_at_unix_ms: u64) -> Self {
        Self {
            timeline: TimelineId::initial(),
            parent_timeline: None,
            fork_lsn: 0,
            created_at_unix_ms,
            reason: "initial".into(),
        }
    }

    pub fn fork(
        timeline: TimelineId,
        parent_timeline: TimelineId,
        fork_lsn: u64,
        created_at_unix_ms: u64,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            timeline,
            parent_timeline: Some(parent_timeline),
            fork_lsn,
            created_at_unix_ms,
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineHistory {
    pub entries: Vec<TimelineHistoryEntry>,
}

impl TimelineHistory {
    pub fn new(initial_created_at_unix_ms: u64) -> Self {
        Self {
            entries: vec![TimelineHistoryEntry::initial(initial_created_at_unix_ms)],
        }
    }

    pub fn current(&self) -> Option<TimelineId> {
        self.entries.last().map(|entry| entry.timeline)
    }

    pub fn fork(
        &mut self,
        new_timeline: TimelineId,
        parent_timeline: TimelineId,
        fork_lsn: u64,
        created_at_unix_ms: u64,
        reason: impl Into<String>,
    ) -> RdbFileResult<()> {
        if self.current() != Some(parent_timeline) {
            return Err(RdbFileError::InvalidOperation(format!(
                "cannot fork timeline {} from non-current parent {}",
                new_timeline.0, parent_timeline.0
            )));
        }
        if new_timeline <= parent_timeline {
            return Err(RdbFileError::InvalidOperation(format!(
                "new timeline {} must be greater than parent {}",
                new_timeline.0, parent_timeline.0
            )));
        }
        self.entries.push(TimelineHistoryEntry::fork(
            new_timeline,
            parent_timeline,
            fork_lsn,
            created_at_unix_ms,
            reason,
        ));
        Ok(())
    }

    pub fn ancestor_lsn(&self, timeline: TimelineId) -> Option<u64> {
        self.entries
            .iter()
            .find(|entry| entry.timeline == timeline)
            .map(|entry| entry.fork_lsn)
    }

    pub fn descendant_chain_from(&self, timeline: TimelineId) -> Option<Vec<TimelineHistoryEntry>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.timeline == timeline)?;
        Some(self.entries[index.saturating_add(1)..].to_vec())
    }

    pub fn rejoin_decision(
        &self,
        node_timeline: TimelineId,
        node_flushed_lsn: u64,
        available_from_lsn: u64,
    ) -> RejoinDecision {
        if self.current() == Some(node_timeline) {
            return RejoinDecision::AlreadyCurrent;
        }
        let Some(current) = self.current() else {
            return RejoinDecision::Reclone;
        };
        let Some(chain) = self.descendant_chain_from(node_timeline) else {
            return RejoinDecision::Reclone;
        };
        let Some(first_child) = chain.first() else {
            return RejoinDecision::Reclone;
        };
        if node_flushed_lsn >= first_child.fork_lsn {
            RejoinDecision::Rewind {
                target_timeline: current,
                rewind_to_lsn: first_child.fork_lsn,
            }
        } else if node_flushed_lsn >= available_from_lsn {
            RejoinDecision::FollowNewTimeline {
                target_timeline: current,
                start_lsn: node_flushed_lsn,
            }
        } else {
            RejoinDecision::Reclone
        }
    }

    pub fn promotion_history(
        &self,
        candidate: &PromotionCandidate,
        new_timeline: TimelineId,
        created_at_unix_ms: u64,
    ) -> RdbFileResult<Self> {
        let mut next = self.clone();
        next.fork(
            new_timeline,
            candidate.timeline,
            candidate.applied_lsn,
            created_at_unix_ms,
            format!("promote {}", candidate.replica_id),
        )?;
        Ok(next)
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> RdbFileResult<()> {
        write_bytes_atomically(path.as_ref(), &self.encode())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> RdbFileResult<Self> {
        Self::decode(&fs::read(path)?)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(TIMELINE_HISTORY_MAGIC);
        put_u16(&mut out, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u32(&mut out, self.entries.len() as u32);
        for entry in &self.entries {
            put_u64(&mut out, entry.timeline.0);
            match entry.parent_timeline {
                Some(parent) => {
                    out.push(1);
                    put_u64(&mut out, parent.0);
                }
                None => {
                    out.push(0);
                    put_u64(&mut out, 0);
                }
            }
            put_u64(&mut out, entry.fork_lsn);
            put_u64(&mut out, entry.created_at_unix_ms);
            put_string(&mut out, &entry.reason);
        }
        let checksum = crc32(&out);
        put_u32(&mut out, checksum);
        out
    }

    pub fn decode(bytes: &[u8]) -> RdbFileResult<Self> {
        verify_checksum(bytes, "timeline history")?;
        let payload_end = bytes.len() - CHECKSUM_LEN;
        let mut cursor = 0usize;
        expect_magic(
            bytes,
            &mut cursor,
            payload_end,
            TIMELINE_HISTORY_MAGIC,
            "timeline history",
        )?;
        let version = take_u16(bytes, &mut cursor, payload_end)?;
        if version != PRIMARY_REPLICA_ARTIFACT_VERSION {
            return Err(RdbFileError::InvalidOperation(format!(
                "unsupported timeline history version {version}"
            )));
        }
        let count = take_u32(bytes, &mut cursor, payload_end)? as usize;
        if count == 0 {
            return Err(RdbFileError::InvalidOperation(
                "timeline history must contain an initial entry".into(),
            ));
        }
        let mut entries = Vec::with_capacity(count);
        let mut previous = None;
        for index in 0..count {
            let timeline = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
            let has_parent = take_u8(bytes, &mut cursor, payload_end)? != 0;
            let parent_raw = TimelineId(take_u64(bytes, &mut cursor, payload_end)?);
            let parent_timeline = has_parent.then_some(parent_raw);
            let fork_lsn = take_u64(bytes, &mut cursor, payload_end)?;
            let created_at_unix_ms = take_u64(bytes, &mut cursor, payload_end)?;
            let reason = take_string(bytes, &mut cursor, payload_end)?;
            if index == 0 && parent_timeline.is_some() {
                return Err(RdbFileError::InvalidOperation(
                    "initial timeline history entry cannot have a parent".into(),
                ));
            }
            if index == 0 && timeline != TimelineId::initial() {
                return Err(RdbFileError::InvalidOperation(
                    "timeline history must start at initial timeline".into(),
                ));
            }
            if index > 0 && parent_timeline != previous {
                return Err(RdbFileError::InvalidOperation(
                    "timeline history parent does not match previous timeline".into(),
                ));
            }
            if let Some(parent) = parent_timeline {
                if timeline <= parent {
                    return Err(RdbFileError::InvalidOperation(format!(
                        "timeline {} must be greater than parent {}",
                        timeline.0, parent.0
                    )));
                }
            }
            previous = Some(timeline);
            entries.push(TimelineHistoryEntry {
                timeline,
                parent_timeline,
                fork_lsn,
                created_at_unix_ms,
                reason,
            });
        }
        reject_trailing_bytes(bytes, cursor, payload_end, "timeline history")?;
        Ok(Self { entries })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejoinDecision {
    AlreadyCurrent,
    FollowNewTimeline {
        target_timeline: TimelineId,
        start_lsn: u64,
    },
    Rewind {
        target_timeline: TimelineId,
        rewind_to_lsn: u64,
    },
    Reclone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionCandidate {
    pub replica_id: String,
    pub timeline: TimelineId,
    pub received_lsn: u64,
    pub flushed_lsn: u64,
    pub applied_lsn: u64,
}

impl PromotionCandidate {
    pub fn select(timeline: TimelineId, acks: &[ReplicaAck]) -> Option<Self> {
        acks.iter()
            .filter(|ack| ack.timeline == timeline)
            .max_by(|left, right| {
                (
                    left.applied_lsn,
                    left.flushed_lsn,
                    left.received_lsn,
                    std::cmp::Reverse(left.replica_id.as_str()),
                )
                    .cmp(&(
                        right.applied_lsn,
                        right.flushed_lsn,
                        right.received_lsn,
                        std::cmp::Reverse(right.replica_id.as_str()),
                    ))
            })
            .map(|ack| Self {
                replica_id: ack.replica_id.clone(),
                timeline: ack.timeline,
                received_lsn: ack.received_lsn,
                flushed_lsn: ack.flushed_lsn,
                applied_lsn: ack.applied_lsn,
            })
    }
}
