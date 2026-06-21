//! CDC enrichment consumer (#1272, PRD #1267) — the first end-to-end AI
//! modality wired over the existing change stream.
//!
//! A collection that declares an `EMBED (...)` policy (issue #1271) gets
//! its declared fields auto-vectorised *asynchronously* after commit. The
//! write path itself does no provider work: an INSERT/UPDATE simply emits
//! its usual CDC event and returns. This consumer is the thing that, on a
//! later pass, drains the LSN-ordered change stream, recomputes embeddings
//! for committed rows via the policy's provider, and attaches the vectors
//! into the collection (reusing the existing `create_vector` +
//! local-embedding machinery).
//!
//! Because a row is only searchable once its vector exists, "pending"
//! enrichment is naturally excluded from `VECTOR SEARCH` until the consumer
//! attaches the vector — at which point the row is included like any other.
//! The consumer additionally owns:
//!   - a `pending` work set (rows whose enrichment hasn't completed),
//!   - retry-with-backoff on provider failure,
//!   - a dead-letter list after a bounded number of failures, and
//!   - an ops re-drive path that moves dead-letters back to pending.
//!
//! The consumer is driven explicitly via [`CdcEnrichmentConsumer::tick`],
//! which takes the current time so retry backoff is deterministic in tests
//! and a production scheduler can drive it from a background thread without
//! changing the semantics.

use crate::application::entity::{CreateVectorInput, PatchEntityInput};
use crate::application::ports::RuntimeEntityPort;
use crate::catalog::{EmbedPolicy, VisionPolicy};
use crate::replication::cdc::ChangeOperation;
use crate::storage::schema::Value;
use crate::storage::{EntityData, EntityId};
use crate::{RedDBError, RedDBResult, RedDBRuntime};

/// Derived field that receives the structured component-detections array
/// (`[{label, confidence, bbox:[x,y,w,h]}]`). It is a normal row field, so
/// RQL filters (e.g. `CONTAINS(vision_detections, 'person')`) work over it
/// once the consumer attaches it.
pub const VISION_DETECTIONS_FIELD: &str = "vision_detections";

/// Which async enrichment a pending work item carries. A single committed
/// row can require both (a collection may declare EMBED and VISION); each
/// is tracked and retried independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichmentKind {
    /// Auto-embed the declared text fields (#1272).
    Embed,
    /// Run computer vision over the declared image-reference field (#1275).
    Vision,
}

/// Tunables for the enrichment consumer.
#[derive(Debug, Clone)]
pub struct EnrichmentConfig {
    /// Number of provider attempts before a work item is dead-lettered.
    /// Must be `>= 1`.
    pub max_attempts: u32,
    /// Base backoff applied after the first failure; subsequent failures
    /// back off exponentially (`base * 2^(attempts-1)`).
    pub base_backoff_ms: u64,
    /// Maximum number of CDC events ingested per `tick`.
    pub poll_max: usize,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_ms: 100,
            poll_max: 1024,
        }
    }
}

/// A row awaiting enrichment.
#[derive(Debug, Clone)]
struct PendingWork {
    collection: String,
    entity_id: u64,
    kind: EnrichmentKind,
    attempts: u32,
    /// Earliest wall-clock (unix ms) at which the next attempt may run.
    not_before_ms: u64,
}

/// A work item that exhausted its retry budget. Surfaced to operators and
/// re-drivable via [`CdcEnrichmentConsumer::redrive`].
#[derive(Debug, Clone)]
pub struct DeadLetter {
    pub collection: String,
    pub entity_id: u64,
    pub kind: EnrichmentKind,
    pub attempts: u32,
    pub last_error: String,
}

/// Per-`tick` outcome counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickStats {
    /// CDC events accepted into the pending set this tick.
    pub ingested: usize,
    /// Rows whose vectors were attached this tick.
    pub attached: usize,
    /// Failed attempts re-scheduled with backoff this tick.
    pub retried: usize,
    /// Work items dead-lettered this tick.
    pub dead_lettered: usize,
}

/// Drains the CDC stream and enriches embed-policy collections.
///
/// Holds its own cursor, pending set, and dead-letter list — one consumer
/// instance owns the enrichment state for a runtime.
pub struct CdcEnrichmentConsumer {
    cursor: u64,
    config: EnrichmentConfig,
    pending: Vec<PendingWork>,
    dead_letters: Vec<DeadLetter>,
}

impl CdcEnrichmentConsumer {
    /// New consumer starting from the stream origin (LSN 0) with the given
    /// config.
    pub fn new(config: EnrichmentConfig) -> Self {
        Self {
            cursor: 0,
            config,
            pending: Vec::new(),
            dead_letters: Vec::new(),
        }
    }

    /// New consumer with default tunables.
    pub fn with_defaults() -> Self {
        Self::new(EnrichmentConfig::default())
    }

    /// Last CDC LSN this consumer has ingested.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Rows currently awaiting enrichment.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// True while `(collection, entity_id)` is still awaiting any kind of
    /// enrichment.
    pub fn is_pending(&self, collection: &str, entity_id: u64) -> bool {
        self.pending
            .iter()
            .any(|w| w.collection == collection && w.entity_id == entity_id)
    }

    /// True while `(collection, entity_id)` is awaiting the given kind of
    /// enrichment specifically.
    pub fn is_pending_kind(&self, collection: &str, entity_id: u64, kind: EnrichmentKind) -> bool {
        self.pending
            .iter()
            .any(|w| w.collection == collection && w.entity_id == entity_id && w.kind == kind)
    }

    /// Dead-lettered work items (enrichment failed past the retry budget).
    pub fn dead_letters(&self) -> &[DeadLetter] {
        &self.dead_letters
    }

    /// Ops re-drive: move every dead-letter back into the pending set with a
    /// fresh retry budget. Returns the number of items re-driven.
    pub fn redrive(&mut self) -> usize {
        let drained: Vec<DeadLetter> = self.dead_letters.drain(..).collect();
        let count = drained.len();
        for dl in drained {
            self.enqueue(dl.collection, dl.entity_id, dl.kind);
        }
        count
    }

    /// Ingest newly committed CDC events and then attempt every pending item
    /// whose backoff has elapsed. Embedding work happens here, never on the
    /// write path — so write latency is independent of provider latency.
    pub fn tick(&mut self, rt: &RedDBRuntime, now_ms: u64) -> RedDBResult<TickStats> {
        let mut stats = TickStats::default();

        // 1. Ingest committed change events for embed- and vision-policy
        //    collections. A row can require both; each modality is queued
        //    and retried independently.
        let events = rt.cdc_poll(self.cursor, self.config.poll_max);
        for event in &events {
            if event.lsn > self.cursor {
                self.cursor = event.lsn;
            }
            if let Some(policy) = rt.collection_embed_policy(&event.collection) {
                if change_touches_embed_fields(event, &policy)
                    && self.enqueue(
                        event.collection.clone(),
                        event.entity_id,
                        EnrichmentKind::Embed,
                    )
                {
                    stats.ingested += 1;
                }
            }
            if let Some(policy) = rt.collection_vision_policy(&event.collection) {
                if change_touches_vision_field(event, &policy)
                    && self.enqueue(
                        event.collection.clone(),
                        event.entity_id,
                        EnrichmentKind::Vision,
                    )
                {
                    stats.ingested += 1;
                }
            }
        }

        // 2. Attempt every ready pending item.
        let drained: Vec<PendingWork> = std::mem::take(&mut self.pending);
        let mut still_pending = Vec::with_capacity(drained.len());
        for mut work in drained {
            if work.not_before_ms > now_ms {
                still_pending.push(work);
                continue;
            }
            // The policy can disappear if the collection was dropped/altered
            // between enqueue and drain — quietly forget such work.
            let attempt = match work.kind {
                EnrichmentKind::Embed => match rt.collection_embed_policy(&work.collection) {
                    Some(policy) => {
                        rt.enrich_row_embedding(&work.collection, work.entity_id, &policy)
                    }
                    None => continue,
                },
                EnrichmentKind::Vision => match rt.collection_vision_policy(&work.collection) {
                    Some(policy) => rt.enrich_row_vision(&work.collection, work.entity_id, &policy),
                    None => continue,
                },
            };
            match attempt {
                Ok(()) => stats.attached += 1,
                Err(err) => {
                    work.attempts += 1;
                    if work.attempts >= self.config.max_attempts {
                        self.dead_letters.push(DeadLetter {
                            collection: work.collection,
                            entity_id: work.entity_id,
                            kind: work.kind,
                            attempts: work.attempts,
                            last_error: format!("{err:?}"),
                        });
                        stats.dead_lettered += 1;
                    } else {
                        let shift = work.attempts - 1;
                        let backoff = self
                            .config
                            .base_backoff_ms
                            .saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
                        work.not_before_ms = now_ms.saturating_add(backoff);
                        still_pending.push(work);
                        stats.retried += 1;
                    }
                }
            }
        }
        self.pending = still_pending;

        Ok(stats)
    }

    /// Add a row to the pending set unless this `(collection, entity, kind)`
    /// is already queued. Returns true when a new item was enqueued.
    fn enqueue(&mut self, collection: String, entity_id: u64, kind: EnrichmentKind) -> bool {
        if self
            .pending
            .iter()
            .any(|w| w.entity_id == entity_id && w.kind == kind && w.collection == collection)
        {
            return false;
        }
        self.pending.push(PendingWork {
            collection,
            entity_id,
            kind,
            attempts: 0,
            not_before_ms: 0,
        });
        true
    }
}

/// Whether a change event should (re)enrich the row under `policy`.
///
/// Inserts always enrich. Updates enrich when the damage vector intersects
/// the declared embed fields, or when no damage vector is available (the
/// emitter didn't compute one — enrich conservatively). Deletes/refreshes
/// never enrich.
fn change_touches_embed_fields(
    event: &crate::replication::cdc::ChangeEvent,
    policy: &EmbedPolicy,
) -> bool {
    match event.operation {
        ChangeOperation::Insert => true,
        ChangeOperation::Update => match &event.changed_columns {
            Some(columns) => columns
                .iter()
                .any(|column| policy.fields.iter().any(|field| field == column)),
            None => true,
        },
        ChangeOperation::Delete | ChangeOperation::Refresh => false,
    }
}

/// Whether a change event should (re)run vision over the row under
/// `policy`. Inserts always run. Updates run only when the declared
/// image-reference field changed (or no damage vector is available, so we
/// run conservatively). Crucially, an update that only touched the derived
/// detections field — the consumer's own write-back — does NOT match,
/// because that field is not the image-reference field, so vision never
/// re-triggers itself into a loop. Deletes/refreshes never run.
fn change_touches_vision_field(
    event: &crate::replication::cdc::ChangeEvent,
    policy: &VisionPolicy,
) -> bool {
    match event.operation {
        ChangeOperation::Insert => true,
        ChangeOperation::Update => match &event.changed_columns {
            Some(columns) => columns.iter().any(|column| column == &policy.image_field),
            None => true,
        },
        ChangeOperation::Delete | ChangeOperation::Refresh => false,
    }
}

/// Whether the policy's output kinds request structured component
/// detections. Several spellings are accepted so DDL authors are not
/// boxed into one keyword.
fn vision_wants_detections(policy: &VisionPolicy) -> bool {
    policy.output_kinds.iter().any(|kind| {
        matches!(
            kind.trim().to_ascii_lowercase().as_str(),
            "detections" | "objects" | "components" | "detection"
        )
    })
}

/// Whether the policy's output kinds request an image-embedding output.
fn vision_wants_embedding(policy: &VisionPolicy) -> bool {
    policy.output_kinds.iter().any(|kind| {
        matches!(
            kind.trim().to_ascii_lowercase().as_str(),
            "embedding" | "image_embedding" | "image-embedding"
        )
    })
}

impl RedDBRuntime {
    /// The declared embed policy for `collection`, if any.
    pub fn collection_embed_policy(&self, collection: &str) -> Option<EmbedPolicy> {
        self.db()
            .collection_contract_arc(collection)
            .and_then(|contract| contract.ai_policy.as_ref().and_then(|p| p.embed.clone()))
    }

    /// The declared vision policy for `collection`, if any.
    pub fn collection_vision_policy(&self, collection: &str) -> Option<VisionPolicy> {
        self.db()
            .collection_contract_arc(collection)
            .and_then(|contract| contract.ai_policy.as_ref().and_then(|p| p.vision.clone()))
    }

    /// Compute the embedding for one committed row and attach it as a vector
    /// in the same collection. Reuses the existing embedding + vector
    /// storage path so `VECTOR SEARCH` surfaces the row exactly as a manual
    /// `WITH AUTO EMBED` insert would.
    ///
    /// A row whose declared fields are all empty is treated as complete (no
    /// vector attached) rather than failed — there is nothing to embed.
    pub(crate) fn enrich_row_embedding(
        &self,
        collection: &str,
        entity_id: u64,
        policy: &EmbedPolicy,
    ) -> RedDBResult<()> {
        let db = self.db();
        // The CDC event carries the row's stable *logical* id; resolve the
        // live version through it so an update re-embeds the new field values
        // rather than a superseded MVCC version. A `None` here means the
        // event was not a live table row (e.g. the enrichment vector's own
        // insert event, or a deleted row) — nothing to enrich.
        let Some(entity) = db
            .store()
            .get_table_row_by_logical_id(collection, EntityId::new(entity_id))
        else {
            return Ok(());
        };

        let Some(text) = combine_embed_text(&entity.data, &policy.fields) else {
            return Ok(());
        };

        let dense = embed_one(self, policy, &text)?;
        if dense.is_empty() {
            return Ok(());
        }

        self.create_vector(CreateVectorInput {
            collection: collection.to_string(),
            dense,
            content: Some(text),
            metadata: Vec::new(),
            link_row: None,
            link_node: None,
        })?;
        Ok(())
    }

    /// Run computer vision over one committed row: fetch the image
    /// referenced by the policy's `image_field`, call the vision provider,
    /// write the structured component-detections to the derived
    /// [`VISION_DETECTIONS_FIELD`] (RQL-filterable), and — when the policy
    /// requests it — attach an image-embedding vector reusing the existing
    /// vector pipeline.
    ///
    /// A row whose image reference is absent/empty is treated as complete
    /// (nothing to analyze) rather than failed.
    pub(crate) fn enrich_row_vision(
        &self,
        collection: &str,
        entity_id: u64,
        policy: &VisionPolicy,
    ) -> RedDBResult<()> {
        // This slice drives the in-process `local` provider (the path the
        // mock vision backend exercises); other providers are rejected with
        // a deterministic error that the retry/dead-letter machinery
        // handles like any failure.
        match crate::ai::parse_provider(&policy.provider)? {
            crate::ai::AiProvider::Local => {}
            other => {
                return Err(RedDBError::Query(format!(
                    "CDC vision enrichment currently drives the 'local' provider; \
                     collection policy declares '{other:?}'"
                )));
            }
        }

        let db = self.db();
        // Resolve the live row through its stable logical id (see
        // `enrich_row_embedding`). `None` means the event was not a live
        // table row — nothing to enrich.
        let Some(entity) = db
            .store()
            .get_table_row_by_logical_id(collection, EntityId::new(entity_id))
        else {
            return Ok(());
        };

        let Some(reference) = row_text_field(&entity.data, &policy.image_field) else {
            return Ok(());
        };
        if reference.is_empty() {
            return Ok(());
        }

        let want_detections = vision_wants_detections(policy);
        let want_embedding = vision_wants_embedding(policy);
        if !want_detections && !want_embedding {
            return Ok(());
        }

        let image_bytes = crate::runtime::ai::vision::fetch_image_bytes(&reference)?;
        let result = crate::runtime::ai::vision::analyze_local(
            &policy.model,
            image_bytes,
            want_detections,
            want_embedding,
        )?;

        if want_detections {
            // Write the canonical detections array as a JSON row field. The
            // damage vector for this update covers only the derived field,
            // never `image_field`, so it cannot re-trigger vision.
            let detections_json = detections_to_json(&result.detections);
            self.patch_entity(PatchEntityInput {
                collection: collection.to_string(),
                id: entity.id,
                payload: vision_detections_payload(detections_json),
                operations: Vec::new(),
            })?;
        }

        if want_embedding {
            if let Some(embedding) = result.embedding {
                if !embedding.is_empty() {
                    self.create_vector(CreateVectorInput {
                        collection: collection.to_string(),
                        dense: embedding,
                        content: Some(reference),
                        metadata: Vec::new(),
                        link_row: None,
                        link_node: None,
                    })?;
                }
            }
        }

        Ok(())
    }
}

/// Read a row's text-valued field as an owned string. Returns `None` when
/// the entity is not a row, the field is absent, or it is not text.
fn row_text_field(data: &EntityData, field: &str) -> Option<String> {
    let EntityData::Row(row) = data else {
        return None;
    };
    let named = row.named.as_ref()?;
    match named.get(field) {
        Some(Value::Text(text)) => Some(text.to_string()),
        Some(Value::Url(url)) => Some(url.clone()),
        _ => None,
    }
}

/// Encode the detections as a JSON array value
/// (`[{label, confidence, bbox:[x,y,w,h]}]`).
fn detections_to_json(
    detections: &[crate::runtime::ai::vision::VisionDetection],
) -> crate::serde_json::Value {
    use crate::serde_json::{Map, Value as Sj};
    let items = detections
        .iter()
        .map(|d| {
            let mut obj = Map::new();
            obj.insert("label".to_string(), Sj::String(d.label.clone()));
            obj.insert("confidence".to_string(), Sj::Number(d.confidence as f64));
            obj.insert(
                "bbox".to_string(),
                Sj::Array(d.bbox.iter().map(|v| Sj::Number(*v as f64)).collect()),
            );
            Sj::Object(obj)
        })
        .collect();
    Sj::Array(items)
}

/// Build the JSON-patch payload that sets the derived detections field via
/// `patch_entity`'s `fields` merge form.
fn vision_detections_payload(
    detections_json: crate::serde_json::Value,
) -> crate::serde_json::Value {
    use crate::serde_json::{Map, Value as Sj};
    let mut fields = Map::new();
    fields.insert(VISION_DETECTIONS_FIELD.to_string(), detections_json);
    let mut root = Map::new();
    root.insert("fields".to_string(), Sj::Object(fields));
    Sj::Object(root)
}

/// Join the declared embed fields' text values, mirroring the manual
/// `WITH AUTO EMBED` collector. Returns `None` when no non-empty text field
/// is present (e.g. the entity is a vector/non-row, or all fields are empty).
fn combine_embed_text(data: &EntityData, fields: &[String]) -> Option<String> {
    let EntityData::Row(row) = data else {
        return None;
    };
    let named = row.named.as_ref()?;
    let texts: Vec<String> = fields
        .iter()
        .filter_map(|field| match named.get(field) {
            Some(Value::Text(t)) if !t.is_empty() => Some(t.to_string()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join(" "))
    }
}

/// Dispatch a single embedding through the policy's provider. This slice
/// drives the in-process `local` backend (the path the issue's mock
/// provider exercises); other providers are rejected with a deterministic
/// error that the retry/dead-letter machinery handles like any failure.
fn embed_one(rt: &RedDBRuntime, policy: &EmbedPolicy, text: &str) -> RedDBResult<Vec<f32>> {
    let provider = crate::ai::parse_provider(&policy.provider)?;
    match provider {
        crate::ai::AiProvider::Local => {
            let db = rt.db();
            let response = crate::runtime::ai::local_embedding::embed_local_with_db(
                &db,
                &policy.model,
                vec![text.to_string()],
            )?;
            response.embeddings.into_iter().next().ok_or_else(|| {
                RedDBError::Query("local embedding backend returned no vector".to_string())
            })
        }
        other => Err(RedDBError::Query(format!(
            "CDC enrichment currently drives the 'local' provider; \
             collection policy declares '{other:?}'"
        ))),
    }
}
