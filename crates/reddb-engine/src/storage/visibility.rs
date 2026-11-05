//! Visibility of persisted entities under an explicit statement snapshot.
//! Host authorization, tenant and provider policy remain outside this module.
use super::entity::{EntityData, UnifiedEntity};
use crate::transaction::read::{record_serializable_read, visibility_check, SnapshotContext};
use reddb_types::Value;

/// Reserved row field that carries a row's moderation visibility state.
///
/// A row that committed but is hidden from normal reads carries this
/// field; a fully-cleared (allowed) row does not. Two values are used:
///   * [`MODERATION_STATUS_PENDING`] — quarantine-pending (provider was
///     down at write time under fail-open; awaiting async re-moderation),
///   * [`MODERATION_STATUS_REJECTED`] — re-moderated to a reject; the row
///     is tombstoned-and-retained for audit/appeal.
///
/// It is a reserved system field (see the host reserved-field registry) so users
/// cannot declare or set it, and the read-path visibility helpers hide any
/// row that carries it from normal SELECT/scan reads (ADR 0057).
pub const MODERATION_STATUS_FIELD: &str = "__moderation_status";

/// Value of [`MODERATION_STATUS_FIELD`] for a quarantine-pending row.
pub const MODERATION_STATUS_PENDING: &str = "pending";

/// Value of [`MODERATION_STATUS_FIELD`] for a rejected-tombstone row.
pub const MODERATION_STATUS_REJECTED: &str = "rejected";

/// Whether `entity` carries a moderation status that must hide it from
/// normal reads (quarantine-pending or rejected-tombstone).
///
/// This is consulted on the hot read path for *every* table-row candidate,
/// so it is a single, allocation-free field probe. Non-row entities and
/// rows without the marker are never hidden by moderation.
#[inline]
pub fn entity_moderation_hidden(entity: &UnifiedEntity) -> bool {
    let EntityData::Row(row) = &entity.data else {
        return false;
    };
    // Hidden only for the two active hidden states. A cleared (allowed)
    // row carries an empty/absent marker and stays visible; this keeps the
    // clear path a simple field overwrite (the storage merge has no
    // field-removal form) without leaking a stale "hidden" decision.
    matches!(
        row.get_field(MODERATION_STATUS_FIELD),
        Some(Value::Text(status))
            if status.as_ref() == MODERATION_STATUS_PENDING
                || status.as_ref() == MODERATION_STATUS_REJECTED
    )
}

/// Apply moderation markers, captured MVCC state and serializable read tracking.
/// With no snapshot this retains the native raw-visibility helper's behavior;
/// collection scans additionally hide superseded physical versions.
#[inline]
pub fn entity_visible_with_context(ctx: Option<&SnapshotContext>, entity: &UnifiedEntity) -> bool {
    // Same moderation visibility gate as the thread-local path (#1274):
    // parallel scan workers capture the snapshot context but must apply
    // the moderation marker check identically.
    if entity_moderation_hidden(entity) {
        return false;
    }
    match ctx {
        Some(ctx) => {
            let visible = visibility_check(ctx, entity.xmin, entity.xmax);
            if visible {
                record_serializable_read(ctx, entity);
            }
            visible
        }
        None => true,
    }
}
