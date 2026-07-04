//! DML execution: INSERT, UPDATE, DELETE via SQL AST
//!
//! Implements `execute_insert`, `execute_update`, and `execute_delete` on
//! `RedDBRuntime`.  Each method translates the parsed AST into entity-level
//! operations through the existing `RuntimeEntityPort` trait so that all
//! cross-cutting concerns (WAL, indexing, replication) are automatically
//! applied.

use crate::application::entity::{
    metadata_from_json, AppliedEntityMutation, CreateDocumentInput, CreateEdgeInput,
    CreateEntityOutput, CreateKvInput, CreateNodeInput, CreateRowInput, CreateRowsBatchInput,
    CreateVectorInput, DeleteEntityInput, PatchEntityOperation, PatchEntityOperationType,
    RowUpdateColumnRule, RowUpdateContractPlan,
};
use crate::application::ports::{
    build_row_update_contract_plan, entity_row_fields_snapshot,
    normalize_row_update_assignment_with_plan, normalize_row_update_value_for_rule,
    RuntimeEntityPort,
};
use crate::application::ttl_payload::has_internal_ttl_metadata;
use crate::presentation::entity_json::storage_value_to_json;
use crate::runtime::mvcc::current_connection_id;
use crate::storage::query::ast::{BinOp, Expr, FieldRef, ReturningItem, UpdateTarget};
use crate::storage::query::sql_lowering::{
    effective_delete_filter, effective_insert_rows, effective_update_filter, fold_expr_to_value,
};
use crate::storage::query::unified::{
    sys_key_collection, sys_key_created_at, sys_key_kind, sys_key_rid, sys_key_tenant,
    sys_key_updated_at, UnifiedRecord, UnifiedResult,
};
use crate::storage::unified::MetadataValue;
use crate::storage::Metadata;
use std::collections::HashMap;
use std::sync::Arc;

use super::*;
// Insert-support and value-conversion helpers were extracted to
// `impl_dml_support` (issue #1632); import them so existing call sites keep
// using their bare names unchanged.
use super::impl_dml_support::*;
// RETURNING / update-analysis / update-target / claim helpers were extracted
// to sibling modules (issue #1633); import them so existing call sites keep
// using their bare names unchanged.
use super::impl_dml_claim::*;
use super::impl_dml_returning::*;
use super::impl_dml_update_analysis::*;
use super::impl_dml_update_target::*;
// Chain-integrity / tenant-injection / batch-flush / crypto method families
// were extracted to sibling modules (issue #1634). Their methods dispatch
// through `self`, so no glob import is needed here. The SQL TTL free functions
// are re-exported so both the execution paths below and
// `impl_dml_support`'s `use super::impl_dml::{...}` keep resolving unchanged.
pub(super) use super::impl_dml_ttl::{canonicalize_sql_ttl_metadata, resolve_sql_ttl_metadata_key};

const UPDATE_APPLY_CHUNK_SIZE: usize = 2048;
pub(super) const TREE_CHILD_EDGE_LABEL: &str = "TREE_CHILD";
pub(super) const TREE_METADATA_PREFIX: &str = "red.tree.";

#[derive(Clone)]
pub(super) struct CompiledUpdateAssignment {
    column: String,
    expr: Expr,
    compound_op: Option<BinOp>,
    metadata_key: Option<&'static str>,
    row_rule: Option<RowUpdateColumnRule>,
}

pub(super) struct CompiledUpdatePlan {
    pub(super) static_field_assignments: Vec<(String, Value)>,
    pub(super) static_metadata_assignments: Vec<(String, MetadataValue)>,
    dynamic_assignments: Vec<CompiledUpdateAssignment>,
    row_contract_plan: Option<RowUpdateContractPlan>,
    row_modified_columns: Vec<String>,
    row_touches_unique_columns: bool,
}

#[derive(Default)]
pub(super) struct MaterializedUpdateAssignments {
    pub(super) dynamic_field_assignments: Vec<(String, Value)>,
    pub(super) dynamic_metadata_assignments: Vec<(String, MetadataValue)>,
}

impl RedDBRuntime {
    /// ADR 0067 (#1710): resolve the model of an unmarked bare-VALUES
    /// INSERT from the catalog, making the marker rule real — *a model
    /// marker exists only where it disambiguates what the catalog cannot
    /// know.*
    ///
    /// `INSERT INTO c VALUES ({…})` has no column list and no model marker,
    /// so the parser leaves `entity_type = Row` with an empty column list.
    /// Only that exact shape is a candidate for inference — every other
    /// INSERT (an explicit column list, or an explicit `DOCUMENT` / `NODE`
    /// / `VECTOR` / … marker) is returned untouched.
    ///
    /// * existing **document** collection → rewritten to a `DOCUMENT`
    ///   insert so the body routes through document creation, exactly the
    ///   path the explicit marker takes;
    /// * existing **non-document** collection → the bare form does not
    ///   apply; reported with a model-oriented error;
    /// * unknown collection → didactic error naming both recourses (the
    ///   `DOCUMENT` assertion form or `CREATE DOCUMENT` first).
    fn infer_unmarked_document_insert(
        &self,
        query: &InsertQuery,
    ) -> RedDBResult<Option<InsertQuery>> {
        if !matches!(query.entity_type, InsertEntityType::Row) || !query.columns.is_empty() {
            return Ok(None);
        }
        match self
            .db()
            .collection_contract_arc(&query.table)
            .map(|contract| contract.declared_model)
        {
            Some(crate::catalog::CollectionModel::Document) => {
                let mut rewritten = query.clone();
                rewritten.entity_type = InsertEntityType::Document;
                rewritten.columns = vec!["body".to_string()];
                Ok(Some(rewritten))
            }
            Some(other) => Err(RedDBError::InvalidOperation(format!(
                "collection '{table}' is declared as '{model}'; the bare \
                 `INSERT INTO {table} VALUES (…)` form is the document body shorthand — \
                 write an explicit column list (`INSERT INTO {table} (col, …) VALUES (…)`) \
                 to insert into a {model} collection",
                table = query.table,
                model = crate::runtime::ddl::polymorphic_resolver::model_name(other),
            ))),
            None => Err(RedDBError::InvalidOperation(format!(
                "collection '{table}' does not exist and the INSERT carries no model marker; \
                 write `INSERT INTO {table} DOCUMENT VALUES ({{…}})` to create it as a document \
                 (idempotent), or run `CREATE DOCUMENT {table}` first",
                table = query.table,
            ))),
        }
    }

    /// Execute INSERT INTO table [entity_type] (cols) VALUES (vals), ...
    ///
    /// Each row in `query.values` is zipped with `query.columns` to produce a
    /// set of named fields, which is then dispatched based on entity_type.
    pub fn execute_insert(
        &self,
        raw_query: &str,
        query: &InsertQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // CollectionContract gate (#49): single entry point for the
        // operator's collection-level write rules. Today this is a
        // no-op for INSERT (APPEND ONLY permits insert); routing
        // through the gate now means future contract bits — versioned,
        // vault-only writes — plug in once instead of per verb.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Insert,
        )?;
        // ADR 0067 (#1710): catalog model inference for the unmarked
        // bare-VALUES INSERT. `INSERT INTO c VALUES ({…})` carries no
        // column list and no model marker (parsed as a Row insert with an
        // empty column list); resolve the model from the catalog so an
        // existing document collection routes to document creation and an
        // unknown collection surfaces a didactic error. Runs before tenant
        // injection so a rewritten document insert is tenant-scoped like
        // the explicit `DOCUMENT` marker path.
        let inferred_owned;
        let query = match self.infer_unmarked_document_insert(query)? {
            Some(rewritten) => {
                inferred_owned = rewritten;
                &inferred_owned
            }
            None => query,
        };
        // Phase 2.5.4 table-scoped tenancy: if the target table is
        // tenant-scoped and the user didn't name the tenant column,
        // auto-inject it with the thread-local `CURRENT_TENANT()`
        // value. When the column is named explicitly we trust the
        // caller (useful for admin tooling that writes on behalf of
        // specific tenants). An unbound tenant on an implicit-fill
        // path errors up front rather than producing a row the RLS
        // policy would silently hide.
        let augmented_owned;
        let query = match self.maybe_inject_tenant_column(query)? {
            Some(new_q) => {
                augmented_owned = new_q;
                &augmented_owned
            }
            None => query,
        };
        self.check_insert_column_policy(query)?;
        if let Some(ref embed_config) = query.auto_embed {
            let provider = crate::ai::parse_provider(&embed_config.provider)?;
            // S3 / #711: planner-level provider gate. Runs before the
            // local-model preflight and the API-key resolver so neither
            // side-effect fires when policy denies.
            crate::runtime::ai::provider_gate::enforce(self, &provider)?;
            if matches!(provider, crate::ai::AiProvider::Local) {
                crate::runtime::ai::local_embedding::ensure_local_embedding_available()?;
                // Issue #682 — pre-flight the local model registry before
                // any row write. Missing model, uninstalled artifacts,
                // wrong task, and disabled-feature failures surface as
                // deterministic errors that leave the target collection
                // untouched, satisfying the "no partial writes on
                // embedding failure" criterion for the failure modes
                // owned by the local provider.
                let model_name = embed_config.model.as_deref().map(str::trim).unwrap_or("");
                if model_name.is_empty() {
                    return Err(RedDBError::Query(
                        "AUTO EMBED with provider=local requires MODEL '<registered-model-name>'; \
                         the local provider does not have an implicit default model"
                            .to_string(),
                    ));
                }
                crate::runtime::ai::local_embedding::preflight_local_embedding(
                    &self.inner.db,
                    model_name,
                )?;
            }
        }

        let mut inserted_count: u64 = 0;
        let effective_rows =
            effective_insert_rows(query).map_err(|msg| RedDBError::Query(msg.to_string()))?;

        // Ensure the collection exists (auto-create on first insert).
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(&query.table);
        let declared_model = self
            .db()
            .collection_contract_arc(&query.table)
            .map(|contract| contract.declared_model);

        let mut returning_snapshots: Option<Vec<Vec<(String, Value)>>> =
            if query.returning.is_some() {
                Some(Vec::with_capacity(effective_rows.len()))
            } else {
                None
            };
        let mut returning_result: Option<UnifiedResult> = None;

        if matches!(query.entity_type, InsertEntityType::Row)
            && !matches!(
                declared_model,
                Some(crate::catalog::CollectionModel::TimeSeries)
            )
        {
            // Issue #523 + #524: blockchain collections seal each row into the
            // chain. When the caller omits the reserved columns, the engine
            // auto-fills (#523). When the caller supplies any reserved column,
            // the values are validated against the current tip and a mismatch
            // surfaces a `BlockchainConflict:` error mapped to HTTP 409 (#524).
            //
            // The whole batch runs under a per-collection chain lock so two
            // concurrent submitters can't both bind to the same prev_hash —
            // the loser observes the advanced tip and gets 409 with the new
            // tip so it can retry.
            let chain_mode = crate::runtime::blockchain_kind::is_chain(&store, &query.table);
            let _chain_lock_arc: Option<Arc<parking_lot::Mutex<()>>> = if chain_mode {
                Some(self.inner.rmw_locks.lock_for(&query.table, "__chain__"))
            } else {
                None
            };
            let _chain_guard = _chain_lock_arc.as_ref().map(|m| m.lock());

            // Issue #525 — refuse new blocks if the chain has been marked
            // `integrity = broken` until an admin clears the flag.
            if chain_mode && self.is_chain_integrity_broken(&query.table) {
                return Err(RedDBError::InvalidOperation(format!(
                    "ChainIntegrityBroken: collection '{}' is locked until \
                     POST /collections/{}/clear-integrity-flag is called by an admin",
                    query.table, query.table
                )));
            }

            // Pull the tip from the in-memory cache; fall back to a one-time
            // scan if the cache hasn't seen this collection yet (cold start
            // after restart). Cache is updated below as rows are sealed.
            let mut chain_tip_full: Option<crate::runtime::blockchain_kind::ChainTipFull> =
                if chain_mode {
                    let mut cache = self.inner.chain_tip_cache.lock();
                    if let Some(existing) = cache.get(&query.table) {
                        Some(existing.clone())
                    } else if let Some(scanned) =
                        crate::runtime::blockchain_kind::chain_tip_full(&store, &query.table)
                    {
                        cache.insert(query.table.clone(), scanned.clone());
                        Some(scanned)
                    } else {
                        None
                    }
                } else {
                    None
                };

            let mut rows = Vec::with_capacity(effective_rows.len());
            for row_values in &effective_rows {
                if row_values.len() != query.columns.len() {
                    return Err(RedDBError::Query(format!(
                        "INSERT column count ({}) does not match value count ({})",
                        query.columns.len(),
                        row_values.len()
                    )));
                }
                let (mut fields, mut metadata) =
                    split_insert_metadata(self, &query.columns, row_values)?;
                if chain_mode {
                    use crate::runtime::blockchain_kind::{
                        chain_conflict_error, COL_BLOCK_HEIGHT, COL_HASH, COL_PREV_HASH,
                        COL_TIMESTAMP, RESERVED_COLUMNS,
                    };
                    let supplied_height = fields
                        .iter()
                        .find(|(k, _)| k == COL_BLOCK_HEIGHT)
                        .map(|(_, v)| v.clone());
                    let supplied_prev = fields
                        .iter()
                        .find(|(k, _)| k == COL_PREV_HASH)
                        .map(|(_, v)| v.clone());
                    let supplied_ts = fields
                        .iter()
                        .find(|(k, _)| k == COL_TIMESTAMP)
                        .map(|(_, v)| v.clone());
                    let supplied_hash = fields.iter().any(|(k, _)| k == COL_HASH);
                    let user_supplied_any = supplied_height.is_some()
                        || supplied_prev.is_some()
                        || supplied_ts.is_some()
                        || supplied_hash;

                    fields.retain(|(k, _)| !RESERVED_COLUMNS.contains(&k.as_str()));
                    let payload = crate::runtime::blockchain_kind::canonical_payload(&fields);

                    let (tip_prev_hash, tip_next_height) = match &chain_tip_full {
                        Some(t) => (t.hash, t.height + 1),
                        None => (crate::storage::blockchain::GENESIS_PREV_HASH, 0u64),
                    };
                    let server_now = crate::runtime::blockchain_kind::now_ms();

                    let (use_prev, use_height, use_ts) = if user_supplied_any {
                        // Caller is participating in the chain protocol —
                        // every field must be supplied AND match the tip.
                        if supplied_hash {
                            return Err(chain_conflict_error(
                                tip_next_height.saturating_sub(1),
                                tip_prev_hash,
                                chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                server_now,
                                "hash column is engine-computed and cannot be supplied",
                            ));
                        }
                        let caller_prev = match &supplied_prev {
                            Some(Value::Blob(b)) if b.len() == 32 => {
                                let mut a = [0u8; 32];
                                a.copy_from_slice(b);
                                a
                            }
                            Some(Value::Text(s)) if s.len() == 64 => {
                                // Accept hex-encoded prev_hash so JSON / SQL
                                // callers without literal-blob syntax can
                                // still participate in the chain protocol.
                                let mut a = [0u8; 32];
                                let mut ok = true;
                                for (i, slot) in a.iter_mut().enumerate() {
                                    let pair = &s.as_ref()[i * 2..i * 2 + 2];
                                    match u8::from_str_radix(pair, 16) {
                                        Ok(byte) => *slot = byte,
                                        Err(_) => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                                if !ok {
                                    return Err(chain_conflict_error(
                                        tip_next_height.saturating_sub(1),
                                        tip_prev_hash,
                                        chain_tip_full
                                            .as_ref()
                                            .map(|t| t.timestamp_ms)
                                            .unwrap_or(0),
                                        server_now,
                                        "prev_hash is not valid hex",
                                    ));
                                }
                                a
                            }
                            _ => {
                                return Err(chain_conflict_error(
                                    tip_next_height.saturating_sub(1),
                                    tip_prev_hash,
                                    chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                    server_now,
                                    "prev_hash missing or not a 32-byte Blob",
                                ));
                            }
                        };
                        if caller_prev != tip_prev_hash {
                            return Err(chain_conflict_error(
                                tip_next_height.saturating_sub(1),
                                tip_prev_hash,
                                chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                server_now,
                                "prev_hash does not match current tip",
                            ));
                        }
                        let caller_height = match &supplied_height {
                            Some(Value::UnsignedInteger(v)) => *v,
                            Some(Value::Integer(v)) if *v >= 0 => *v as u64,
                            _ => {
                                return Err(chain_conflict_error(
                                    tip_next_height.saturating_sub(1),
                                    tip_prev_hash,
                                    chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                    server_now,
                                    "block_height missing or not an unsigned integer",
                                ));
                            }
                        };
                        if caller_height != tip_next_height {
                            return Err(chain_conflict_error(
                                tip_next_height.saturating_sub(1),
                                tip_prev_hash,
                                chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                server_now,
                                "block_height does not match tip+1",
                            ));
                        }
                        let caller_ts = match &supplied_ts {
                            Some(Value::UnsignedInteger(v)) => *v,
                            Some(Value::Integer(v)) if *v >= 0 => *v as u64,
                            _ => {
                                return Err(chain_conflict_error(
                                    tip_next_height.saturating_sub(1),
                                    tip_prev_hash,
                                    chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                    server_now,
                                    "timestamp missing or not an unsigned integer",
                                ));
                            }
                        };
                        let drift = (caller_ts as i128) - (server_now as i128);
                        if drift.abs() > 60_000 {
                            return Err(chain_conflict_error(
                                tip_next_height.saturating_sub(1),
                                tip_prev_hash,
                                chain_tip_full.as_ref().map(|t| t.timestamp_ms).unwrap_or(0),
                                server_now,
                                "timestamp outside ±60s of server_time",
                            ));
                        }
                        (caller_prev, caller_height, caller_ts)
                    } else {
                        (tip_prev_hash, tip_next_height, server_now)
                    };

                    let (reserved, new_hash) =
                        crate::runtime::blockchain_kind::make_block_reserved_fields(
                            use_prev, use_height, use_ts, &payload,
                        );
                    fields.extend(reserved);
                    chain_tip_full = Some(crate::runtime::blockchain_kind::ChainTipFull {
                        height: use_height,
                        hash: new_hash,
                        timestamp_ms: use_ts,
                    });
                }
                // Issue #522 — signed-writes verification. On collections
                // created with `SIGNED_BY (...)` the row must carry valid
                // `signer_pubkey` + `signature` reserved columns. Runs
                // after chain_mode so canonical payload covers user-supplied
                // fields only (blockchain reserved columns are filtered by
                // `canonical_payload`; the two signed-writes reserved
                // columns are split out before payload computation, then
                // re-attached for storage). The blockchain + SIGNED_BY
                // composition is owned by issue #526; we keep #522 to the
                // non-chain path and let chain_mode collections punt to that
                // slice rather than half-wire it here.
                if crate::runtime::signed_writes_kind::is_signed(&store, &query.table) {
                    let (pk_col, sig_col, residual) =
                        crate::runtime::signed_writes_kind::split_signature_fields(fields);
                    let payload = crate::runtime::blockchain_kind::canonical_payload(&residual);
                    let reg = crate::runtime::signed_writes_kind::registry(&store, &query.table);
                    crate::runtime::signed_writes_kind::verify_row(
                        &reg,
                        pk_col.as_ref().map(|c| c.bytes.as_slice()),
                        sig_col.as_ref().map(|c| c.bytes.as_slice()),
                        &payload,
                    )
                    .map_err(crate::runtime::signed_writes_kind::map_error)?;
                    fields = residual;
                    // Round-trip the reserved columns with the value
                    // type the caller supplied (Text/hex on the SQL path,
                    // Blob on the binary path). Keeps SELECT and WHERE
                    // predicates symmetric with the INSERT shape.
                    if let Some(col) = pk_col {
                        fields.push((
                            crate::storage::signed_writes::RESERVED_SIGNER_PUBKEY_COL.to_string(),
                            col.raw_value,
                        ));
                    }
                    if let Some(col) = sig_col {
                        fields.push((
                            crate::storage::signed_writes::RESERVED_SIGNATURE_COL.to_string(),
                            col.raw_value,
                        ));
                    }
                }
                merge_with_clauses(
                    &mut metadata,
                    query.ttl_ms,
                    query.expires_at_ms,
                    &query.with_metadata,
                );
                if let Some(snaps) = returning_snapshots.as_mut() {
                    snaps.push(fields.clone());
                }
                rows.push(CreateRowInput {
                    collection: query.table.clone(),
                    fields,
                    metadata,
                    node_links: Vec::new(),
                    vector_links: Vec::new(),
                });
            }
            let outputs = self.create_rows_batch(CreateRowsBatchInput {
                collection: query.table.clone(),
                rows,
                suppress_events: query.suppress_events,
            })?;
            inserted_count = outputs.len() as u64;

            // Chain mode: commit the new tip to the in-memory cache only after
            // the batch persisted successfully. If the batch threw mid-way the
            // cache stays on the previous tip and the chain lock releases.
            if chain_mode {
                if let Some(new_tip) = chain_tip_full.as_ref() {
                    self.inner
                        .chain_tip_cache
                        .lock()
                        .insert(query.table.clone(), new_tip.clone());
                }
            }

            // Hypertable chunk routing: if this table was declared via
            // CREATE HYPERTABLE, register each row's time-column value
            // with the registry so chunk metadata (bounds, row counts,
            // TTL eligibility) stays current. This is what lets
            // HYPERTABLE_PRUNE_CHUNKS answer real questions + lets the
            // retention daemon sweep expired chunks without scanning
            // every row.
            if let Some(spec) = self.inner.db.hypertables().get(&query.table) {
                let time_col = &spec.time_column;
                // Find the column's index in the INSERT column list.
                if let Some(idx) = query.columns.iter().position(|c| c == time_col) {
                    for row in &effective_rows {
                        if let Some(Value::Integer(n) | Value::BigInt(n)) = row.get(idx) {
                            if *n >= 0 {
                                let _ = self.inner.db.hypertables().route(&query.table, *n as u64);
                            }
                        } else if let Some(Value::UnsignedInteger(n)) = row.get(idx) {
                            let _ = self.inner.db.hypertables().route(&query.table, *n);
                        }
                    }
                }
            }

            if let (Some(items), Some(snaps)) =
                (query.returning.as_ref(), returning_snapshots.take())
            {
                let snaps = row_insert_returning_snapshots(&outputs, snaps);
                returning_result = Some(build_returning_result(items, &snaps, Some(&outputs)));
            }
        } else {
            // Issue #419: surface the inserted entity id on every INSERT path.
            // For Node/Edge/Vector/Document/Kv we now keep each CreateEntityOutput
            // so a RETURNING clause (and the unconditional inserted_ids list,
            // below) can expose the engine-assigned id. TimeSeries (the row
            // branch in this else) still returns the not-supported error
            // because create_timeseries_point isn't plumbed through this fn.
            let mut entity_outputs: Vec<crate::application::entity::CreateEntityOutput> =
                Vec::with_capacity(effective_rows.len());
            let mut returning_field_snaps: Vec<Vec<(String, Value)>> = if query.returning.is_some()
            {
                Vec::with_capacity(effective_rows.len())
            } else {
                Vec::new()
            };
            if matches!(
                query.entity_type,
                InsertEntityType::Node | InsertEntityType::Edge
            ) {
                enum PreparedGraphInsert {
                    Node {
                        fields: Vec<(String, Value)>,
                        input: CreateNodeInput,
                    },
                    Edge {
                        fields: Vec<(String, Value)>,
                        input: CreateEdgeInput,
                    },
                }

                let mut prepared = Vec::with_capacity(effective_rows.len());
                for row_values in &effective_rows {
                    if row_values.len() != query.columns.len() {
                        return Err(RedDBError::Query(format!(
                            "INSERT column count ({}) does not match value count ({})",
                            query.columns.len(),
                            row_values.len()
                        )));
                    }

                    match query.entity_type {
                        InsertEntityType::Node => {
                            let (node_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            ensure_non_tree_reserved_metadata_entries(&metadata)?;
                            apply_collection_default_ttl_metadata(
                                self,
                                &query.table,
                                &mut metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&node_values);
                            let label = find_column_value_string(&columns, &values, "label")?;
                            let node_type =
                                find_column_value_opt_string(&columns, &values, "node_type");
                            let properties = extract_remaining_properties(
                                &columns,
                                &values,
                                &["label", "node_type"],
                            );
                            crate::reserved_fields::ensure_no_reserved_public_item_fields(
                                properties.iter().map(|(key, _)| key.as_str()),
                                &format!("node '{}'", query.table),
                            )?;
                            prepared.push(PreparedGraphInsert::Node {
                                fields: node_values,
                                input: CreateNodeInput {
                                    collection: query.table.clone(),
                                    label,
                                    node_type,
                                    properties,
                                    metadata,
                                    embeddings: Vec::new(),
                                    table_links: Vec::new(),
                                    node_links: Vec::new(),
                                },
                            });
                        }
                        InsertEntityType::Edge => {
                            let (edge_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            ensure_non_tree_reserved_metadata_entries(&metadata)?;
                            apply_collection_default_ttl_metadata(
                                self,
                                &query.table,
                                &mut metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&edge_values);
                            let label = find_column_value_string(&columns, &values, "label")?;
                            ensure_non_tree_structural_edge_label(&label)?;
                            let from_id = resolve_edge_endpoint_any(
                                self.inner.db.store().as_ref(),
                                &query.table,
                                &columns,
                                &values,
                                &["from_rid", "from"],
                            )?;
                            let to_id = resolve_edge_endpoint_any(
                                self.inner.db.store().as_ref(),
                                &query.table,
                                &columns,
                                &values,
                                &["to_rid", "to"],
                            )?;
                            let weight = find_column_value_f32_opt(&columns, &values, "weight");
                            let properties = extract_remaining_properties(
                                &columns,
                                &values,
                                &["label", "from_rid", "to_rid", "from", "to", "weight"],
                            );
                            crate::reserved_fields::ensure_no_reserved_public_item_fields(
                                properties.iter().map(|(key, _)| key.as_str()),
                                &format!("edge '{}'", query.table),
                            )?;
                            prepared.push(PreparedGraphInsert::Edge {
                                fields: edge_values,
                                input: CreateEdgeInput {
                                    collection: query.table.clone(),
                                    label,
                                    from: EntityId::new(from_id),
                                    to: EntityId::new(to_id),
                                    weight,
                                    properties,
                                    metadata,
                                },
                            });
                        }
                        _ => unreachable!("prepared graph insert only handles NODE and EDGE"),
                    }
                }

                ensure_graph_insert_contract(self, &query.table)?;
                let mut batch = self.inner.db.batch();
                for item in prepared {
                    match item {
                        PreparedGraphInsert::Node { fields, input } => {
                            if query.returning.is_some() {
                                returning_field_snaps.push(fields);
                            }
                            let node_type = input.node_type.unwrap_or_else(|| input.label.clone());
                            batch = batch.add_node_with_type(
                                input.collection,
                                input.label,
                                node_type,
                                input.properties.into_iter().collect(),
                                input.metadata.into_iter().collect(),
                            );
                        }
                        PreparedGraphInsert::Edge { fields, input } => {
                            if query.returning.is_some() {
                                returning_field_snaps.push(fields);
                            }
                            batch = batch.add_edge(
                                input.collection,
                                input.label,
                                input.from,
                                input.to,
                                input.weight.unwrap_or(1.0),
                                input.properties.into_iter().collect(),
                                input.metadata.into_iter().collect(),
                            );
                        }
                    }
                }
                let batch_result = batch
                    .execute()
                    .map_err(|err| RedDBError::Internal(format!("{err:?}")))?;
                let (ids, entity_kind) = match query.entity_type {
                    InsertEntityType::Node => (batch_result.nodes, "graph_node"),
                    InsertEntityType::Edge => (batch_result.edges, "graph_edge"),
                    _ => unreachable!("prepared graph insert only handles NODE and EDGE"),
                };
                for id in &ids {
                    self.stamp_xmin_if_in_txn(&query.table, *id);
                }
                if query.returning.is_some() {
                    returning_field_snaps = graph_insert_returning_snapshots(
                        self.inner.db.store().as_ref(),
                        &query.table,
                        &ids,
                    );
                }
                self.cdc_emit_insert_batch_no_cache_invalidate(&query.table, &ids, entity_kind);
                let store = self.inner.db.store();
                entity_outputs.extend(ids.iter().map(|id| {
                    crate::application::entity::CreateEntityOutput {
                        id: *id,
                        entity: store.get(&query.table, *id),
                    }
                }));
                inserted_count = ids.len() as u64;
            } else {
                for row_values in &effective_rows {
                    if row_values.len() != query.columns.len() {
                        return Err(RedDBError::Query(format!(
                            "INSERT column count ({}) does not match value count ({})",
                            query.columns.len(),
                            row_values.len()
                        )));
                    }

                    match query.entity_type {
                        InsertEntityType::Row => {
                            if query.returning.is_some() {
                                return Err(RedDBError::Query(
                                "RETURNING is not yet supported for this INSERT path (TimeSeries)"
                                    .to_string(),
                            ));
                            }
                            let (fields, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            self.insert_timeseries_point(&query.table, fields, metadata)?;
                        }
                        InsertEntityType::Node | InsertEntityType::Edge => {
                            unreachable!("NODE and EDGE are handled by the prepared graph path")
                        }
                        InsertEntityType::Vector => {
                            let (vector_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&vector_values);
                            let dense = find_column_value_vec_f32_any(
                                &columns,
                                &values,
                                &["dense", "embedding"],
                            )?;
                            merge_vector_metadata_column(&mut metadata, &columns, &values)?;
                            let content =
                                find_column_value_opt_string(&columns, &values, "content");
                            if query.returning.is_some() {
                                returning_field_snaps.push(vector_values.clone());
                            }
                            let input = CreateVectorInput {
                                collection: query.table.clone(),
                                dense,
                                content,
                                metadata,
                                link_row: None,
                                link_node: None,
                            };
                            entity_outputs.push(self.create_vector(input)?);
                        }
                        InsertEntityType::Document => {
                            let (document_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&document_values);
                            let body = find_document_body_json(&columns, &values)?;
                            let input = CreateDocumentInput {
                                collection: query.table.clone(),
                                body,
                                metadata,
                                node_links: Vec::new(),
                                vector_links: Vec::new(),
                            };
                            let output = self.create_document(input)?;
                            if query.returning.is_some() {
                                let fields = output
                                    .entity
                                    .as_ref()
                                    .map(entity_row_fields_snapshot)
                                    .filter(|fields| !fields.is_empty())
                                    .unwrap_or(document_values);
                                returning_field_snaps.push(fields);
                            }
                            entity_outputs.push(output);
                        }
                        InsertEntityType::Kv => {
                            let (kv_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&kv_values);
                            let key = find_column_value_string(&columns, &values, "key")?;
                            let value = find_column_value(&columns, &values, "value")?;
                            if query.returning.is_some() {
                                returning_field_snaps.push(kv_values.clone());
                            }
                            let input = CreateKvInput {
                                collection: query.table.clone(),
                                key,
                                value,
                                metadata,
                            };
                            entity_outputs.push(self.create_kv(input)?);
                        }
                    }

                    inserted_count += 1;
                }
            }

            if let Some(items) = query.returning.as_ref() {
                if !entity_outputs.is_empty() {
                    returning_result = Some(build_returning_result(
                        items,
                        &returning_field_snaps,
                        Some(&entity_outputs),
                    ));
                }
            }
        }

        // Auto-embed pipeline: batch-embed fields across all inserted rows via AiBatchClient.
        if let Some(ref embed_config) = query.auto_embed {
            let store = self.inner.db.store();
            let provider = crate::ai::parse_provider(&embed_config.provider)?;
            let is_local_provider = matches!(provider, crate::ai::AiProvider::Local);
            // Local provider runs in-process — no API key path applies.
            // The pre-flight above already required `MODEL '<name>'`
            // for the local case, so the unwrap_or default below only
            // ever fires for OpenAI-compatible providers.
            let api_key = if is_local_provider {
                String::new()
            } else {
                crate::ai::resolve_api_key_from_runtime(&provider, None, self)?
            };
            let model = embed_config.model.clone().unwrap_or_else(|| {
                std::env::var("REDDB_OPENAI_EMBEDDING_MODEL")
                    .ok()
                    .unwrap_or_else(|| crate::ai::DEFAULT_OPENAI_EMBEDDING_MODEL.to_string())
            });

            // Collect the just-inserted rows (most-recently appended, reversed back to insert order).
            let manager = store
                .get_collection(&query.table)
                .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
            let entities = manager.query_all(|_| true);
            let recent: Vec<_> = entities
                .into_iter()
                .rev()
                .take(effective_rows.len())
                .collect();

            // Collector phase: (entity_index, combined_text) for rows that have non-empty fields.
            let entity_combos: Vec<(usize, String)> = recent
                .iter()
                .enumerate()
                .filter_map(|(i, entity)| {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let texts: Vec<String> = embed_config
                                .fields
                                .iter()
                                .filter_map(|field| match named.get(field) {
                                    Some(Value::Text(t)) if !t.is_empty() => Some(t.to_string()),
                                    _ => None,
                                })
                                .collect();
                            if !texts.is_empty() {
                                return Some((i, texts.join(" ")));
                            }
                        }
                    }
                    None
                })
                .collect();

            if !entity_combos.is_empty() {
                // Batch phase: single provider round-trip for all rows.
                let batch_texts: Vec<String> =
                    entity_combos.iter().map(|(_, t)| t.clone()).collect();

                // Issue #682 — when the provider is `local`, bypass
                // AiBatchClient (which is HTTP-only) and dispatch
                // directly through the in-process local embedding
                // backend. All texts go in one call, mirroring the
                // single-round-trip shape of the remote path. The
                // local backend does not perform intra-batch dedup —
                // each input position gets its own row in the output
                // — which keeps the per-row "create_vector" loop
                // below correct without additional fan-out logic.
                let embeddings = if is_local_provider {
                    let response = crate::runtime::ai::local_embedding::embed_local_with_db(
                        &self.inner.db,
                        &model,
                        batch_texts,
                    )?;
                    response.embeddings
                } else {
                    let batch_client =
                        crate::runtime::ai::batch_client::AiBatchClient::from_runtime(self);

                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => tokio::task::block_in_place(|| {
                            handle.block_on(batch_client.embed_batch(
                                &provider,
                                &model,
                                &api_key,
                                batch_texts,
                            ))
                        }),
                        Err(_) => {
                            return Err(RedDBError::Query(
                                "AUTO EMBED requires a Tokio runtime context".to_string(),
                            ));
                        }
                    }
                    .map_err(|e| RedDBError::Query(e.to_string()))?
                };

                // Distribute phase: persist one vector per non-empty embedding.
                for ((_, combined), dense) in entity_combos.iter().zip(embeddings) {
                    if dense.is_empty() {
                        continue;
                    }
                    self.create_vector(CreateVectorInput {
                        collection: query.table.clone(),
                        dense,
                        content: Some(combined.clone()),
                        metadata: Vec::new(),
                        link_row: None,
                        link_node: None,
                    })?;
                }
            }
        }

        if inserted_count > 0 {
            self.note_table_write(&query.table);
        }

        let mut result = RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            inserted_count,
            "insert",
            "runtime-dml",
        );
        if let Some(returning) = returning_result {
            result.result = returning;
        }
        Ok(result)
    }

    fn check_insert_column_policy(&self, query: &InsertQuery) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let Some((username, role)) = crate::runtime::impl_core::current_auth_identity() else {
            return Ok(());
        };

        let tenant = crate::runtime::impl_core::current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let request = crate::auth::ColumnAccessRequest {
            action: "insert".to_string(),
            schema: None,
            table: query.table.clone(),
            columns: query.columns.clone(),
        };
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: role == crate::auth::Role::Admin,
            principal_is_platform_scoped: principal.tenant.is_none(),
        };

        let outcome = auth_store.check_column_projection_authz(&principal, &request, &ctx);
        let table_allowed = matches!(
            outcome.table_decision,
            crate::auth::policies::Decision::Allow { .. }
                | crate::auth::policies::Decision::AdminBypass
        );
        if !table_allowed {
            return Err(RedDBError::Query(format!(
                "principal=`{username}` action=`insert` resource=`{}:{}` denied by IAM policy",
                outcome.table_resource.kind, outcome.table_resource.name
            )));
        }
        if let Some(denied) = outcome.first_denied_column() {
            return Err(RedDBError::Query(format!(
                "principal=`{username}` action=`insert` resource=`{}:{}` denied by IAM policy",
                denied.resource.kind, denied.resource.name
            )));
        }

        Ok(())
    }

    pub(crate) fn insert_timeseries_point(
        &self,
        collection: &str,
        fields: Vec<(String, Value)>,
        mut metadata: Vec<(String, MetadataValue)>,
    ) -> RedDBResult<EntityId> {
        apply_collection_default_ttl_metadata(self, collection, &mut metadata);

        let (columns, values) = pairwise_columns_values(&fields);
        validate_timeseries_insert_columns(&columns)?;

        // Issue #577 — AnalyticsSchemaRegistry hook. If the row carries
        // an `event_name` whose schema is registered, validate the
        // `payload` JSON against it BEFORE any write side-effect. On
        // failure we return a typed error and the row is not
        // persisted. When no schema is registered for the event name
        // (or no `event_name` column is supplied at all) we fall
        // through to the normal write path for back-compat with
        // existing timeseries rows.
        let event_name_opt = find_column_value_opt_string(&columns, &values, "event_name");
        let payload_opt = find_column_value_opt_string(&columns, &values, "payload");
        if let Some(event_name) = event_name_opt.as_deref() {
            let store_for_schema = self.inner.db.store();
            if super::analytics_schema_registry::latest(store_for_schema.as_ref(), event_name)
                .is_some()
            {
                let payload_json = payload_opt.as_deref().unwrap_or("{}");
                super::analytics_schema_registry::validate(
                    store_for_schema.as_ref(),
                    event_name,
                    payload_json,
                )
                .map_err(super::analytics_schema_registry::validation_error_to_reddb)?;
            }
        }

        // `metric` is required by the existing timeseries write path;
        // when an analytics-style row supplies `event_name` but not
        // `metric`, fall back to the event name so the storage path
        // still has a non-empty metric tag.
        let metric = match find_column_value_opt_string(&columns, &values, "metric") {
            Some(m) => m,
            None => event_name_opt.clone().ok_or_else(|| {
                RedDBError::Query(
                    "timeseries INSERT requires either `metric` or `event_name`".to_string(),
                )
            })?,
        };
        // `value` is optional for analytics-event rows (which are
        // semantically counts of 1); default to 1.0 when missing so
        // analytics inserts don't have to fabricate a metric value.
        let value = match find_column_value_opt_string(&columns, &values, "value") {
            Some(s) => s.parse::<f64>().unwrap_or(1.0),
            None => columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case("value"))
                .and_then(|i| match &values[i] {
                    Value::Float(f) => Some(*f),
                    Value::Integer(n) | Value::BigInt(n) => Some(*n as f64),
                    Value::UnsignedInteger(n) => Some(*n as f64),
                    _ => None,
                })
                .unwrap_or(1.0),
        };
        let timestamp_ns =
            find_timeseries_timestamp_ns(&columns, &values)?.unwrap_or_else(current_unix_ns);
        let mut tags = find_timeseries_tags(&columns, &values)?;
        if let Some(ref name) = event_name_opt {
            tags.entry("event_name".to_string())
                .or_insert_with(|| name.clone());
        }
        if let Some(ref payload) = payload_opt {
            tags.entry("payload".to_string())
                .or_insert_with(|| payload.clone());
        }

        let mut entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TimeSeriesPoint(Box::new(crate::storage::TimeSeriesPointKind {
                series: collection.to_string(),
                metric: metric.clone(),
            })),
            EntityData::TimeSeries(crate::storage::TimeSeriesData {
                metric,
                timestamp_ns,
                value,
                tags,
            }),
        );
        // MVCC #30: stamp xmin with the active tx xid (inside a tx)
        // or an autocommit xid (allocated and committed up-front so
        // future snapshots see the row as soon as it lands).
        let writer_xid = match self.current_xid() {
            Some(xid) => xid,
            None => {
                let mgr = self.snapshot_manager();
                let xid = mgr.begin();
                mgr.commit(xid);
                xid
            }
        };
        entity.set_xmin(writer_xid);

        let store = self.inner.db.store();
        let id = store
            .insert_auto(collection, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        if !metadata.is_empty() {
            let _ = store.set_metadata(
                collection,
                id,
                Metadata::with_fields(metadata.into_iter().collect()),
            );
        }

        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            collection,
            id.raw(),
            "timeseries",
        );

        Ok(id)
    }

    /// Execute UPDATE table SET col=val, ... WHERE filter
    ///
    /// Scans the target collection, evaluates the WHERE filter against each
    /// record, and patches every matching entity.
    pub fn execute_update(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // Issue #523 — blockchain collections are immutable. Reject before
        // RLS / RETURNING work so the operator sees a clean 409-mapped
        // error instead of a partially-applied mutation surface.
        if crate::runtime::blockchain_kind::is_chain(self.inner.db.store().as_ref(), &query.table) {
            return Err(RedDBError::InvalidOperation(format!(
                "BlockchainCollectionImmutable: UPDATE not allowed on '{}'",
                query.table
            )));
        }
        // Queue-shaped CLAIM (ADR 0020, #1609): a CLAIM on a queue collection
        // is a QueueLifecycle delivery acquisition, not a raw row UPDATE.
        // Route it through the lifecycle seam before the table-shaped
        // contract / RLS gates below (which would reject an UPDATE against a
        // queue's declared model) so QueueLifecycle stays the sole authority
        // for delivery state.
        if query.claim_limit.is_some() && self.is_queue_collection(&query.table) {
            return self.execute_queue_shaped_claim(raw_query, query);
        }
        // CollectionContract gate (#50): runs the APPEND ONLY guard
        // (and any future contract bits) before RLS / RETURNING work
        // so the operator's immutability declaration is honoured
        // uniformly and the error message points at the DDL rather
        // than at a downstream symptom.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Update,
        )?;
        ensure_update_target_contract(self, &query.table, query.target)?;
        ensure_kv_key_update_target_allowed(query)?;
        ensure_graph_identity_update_target_allowed(query)?;

        // Apply RLS augmentation first so every downstream path — plain
        // UPDATE, UPDATE...RETURNING, the inner scan — observes the
        // same policy-filtered target set. This prevents RETURNING
        // from ever exposing rows the UPDATE policy would have
        // denied.
        let rls_gated = crate::runtime::impl_core::rls_is_enabled(self, &query.table);
        let augmented_query: UpdateQuery;
        let effective_query: &UpdateQuery = if rls_gated {
            let update_filter = crate::runtime::impl_core::rls_policy_filter(
                self,
                &query.table,
                crate::storage::query::ast::PolicyAction::Update,
            );
            let Some(mut policy) = update_filter else {
                // No admitting policy: zero rows affected, empty
                // RETURNING (never leak rows the caller can't touch).
                let mut response = RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    0,
                    "update",
                    "runtime-dml-rls",
                );
                if let Some(items) = query.returning.clone() {
                    response.result = build_returning_result(&items, &[], None);
                }
                return Ok(response);
            };
            if query.claim_limit.is_some() {
                let read_filter = crate::runtime::impl_core::rls_policy_filter(
                    self,
                    &query.table,
                    crate::storage::query::ast::PolicyAction::Select,
                );
                let Some(read_policy) = read_filter else {
                    let mut response = RuntimeQueryResult::dml_result(
                        raw_query.to_string(),
                        0,
                        "update",
                        "runtime-dml-rls",
                    );
                    if let Some(items) = query.returning.clone() {
                        response.result = build_returning_result(&items, &[], None);
                    }
                    return Ok(response);
                };
                policy = crate::storage::query::ast::Filter::And(
                    Box::new(read_policy),
                    Box::new(policy),
                );
            }
            let mut augmented = query.clone();
            augmented.filter = Some(match augmented.filter.take() {
                Some(existing) => {
                    crate::storage::query::ast::Filter::And(Box::new(existing), Box::new(policy))
                }
                None => policy,
            });
            augmented_query = augmented;
            &augmented_query
        } else {
            query
        };

        // RETURNING wraps the inner executor and uses the touched-id
        // list the inner reports so the post-image reflects exactly
        // the rows the UPDATE actually mutated (not whatever a
        // separate SELECT might have observed).
        if let Some(items) = effective_query.returning.clone() {
            let mut inner_query = effective_query.clone();
            inner_query.returning = None;
            let (mut response, touched_ids) =
                self.execute_update_inner_tracked(raw_query, &inner_query)?;

            let mut snapshots = if matches!(
                effective_query.target,
                UpdateTarget::Nodes | UpdateTarget::Edges
            ) {
                graph_update_returning_snapshots(self, &effective_query.table, &touched_ids)
            } else {
                super::dml_target_scan::DmlTargetScan::new(self, &effective_query.table, None, None)
                    .row_snapshots(&touched_ids)
            };
            if matches!(effective_query.target, UpdateTarget::Kv) {
                restore_kv_returning_keys(
                    self,
                    &effective_query.table,
                    &touched_ids,
                    &mut snapshots,
                );
            }

            response.result = build_returning_result(&items, &snapshots, None);
            response.engine = "runtime-dml-returning";
            return Ok(response);
        }

        self.execute_update_inner(raw_query, effective_query)
    }

    /// Back-compat shim: the older entry point ignored touched ids.
    fn execute_update_inner(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.execute_update_inner_tracked(raw_query, query)
            .map(|(res, _)| res)
    }

    /// Enforce the concurrent-claim ORDER BY index-gate (ADR 0063, #1607).
    ///
    /// A `CLAIM LIMIT n` / `CLAIM EXACT n` on a logical-identity model (tables
    /// and documents) must order its candidates through a compatible index; the
    /// planner rejects an ordering no index on the collection can serve rather
    /// than falling back to a broad write-path sort. KV (key identity) and graph
    /// nodes/edges are exempt — their claim identity is intrinsic, not a user
    /// ORDER BY column that needs a secondary index.
    fn enforce_claim_order_by_index_gate(&self, query: &UpdateQuery) -> RedDBResult<()> {
        if query.claim_limit.is_none()
            || !matches!(query.target, UpdateTarget::Rows | UpdateTarget::Documents)
        {
            return Ok(());
        }
        let available_indexes: Vec<Vec<String>> = self
            .index_store_ref()
            .list_indices(&query.table)
            .into_iter()
            .map(|index| index.columns)
            .collect();
        reddb_rql::planner::check_claim_order_by_index_gate(query, &available_indexes)
            .map_err(RedDBError::InvalidOperation)
    }

    fn execute_update_inner_tracked(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<(RuntimeQueryResult, Vec<EntityId>)> {
        self.enforce_claim_order_by_index_gate(query)?;
        let store = self.inner.db.store();
        let effective_filter = effective_update_filter(query);
        let compiled_plan = self.compile_update_plan(query)?;
        let needs_rmw_lock = update_needs_rmw_lock(query);
        let claim_model = update_model_name(update_target_model(query.target));
        if query.claim_limit.is_some() {
            self.inner
                .claim_telemetry
                .record_attempt(&query.table, claim_model);
        }
        let claim_lock = query.claim_limit.map(|_| {
            self.inner
                .rmw_locks
                .lock_for(&query.table, "__table_claim_update__")
        });
        let _claim_guard = if let Some(lock) = claim_lock.as_ref() {
            let Some(guard) = lock.try_lock() else {
                let skipped_locked = query.claim_limit.unwrap_or(1);
                self.inner.claim_telemetry.record_skipped_locked(
                    &query.table,
                    claim_model,
                    skipped_locked,
                );
                tracing::debug!(
                    target: "reddb::claim",
                    collection = %query.table,
                    model = claim_model,
                    skipped_locked,
                    "concurrent claim skipped locked candidates"
                );
                return Ok((
                    RuntimeQueryResult::dml_result(
                        raw_query.to_string(),
                        0,
                        "update",
                        "runtime-dml",
                    ),
                    Vec::new(),
                ));
            };
            Some(guard)
        } else {
            None
        };
        let table_rmw_lock = if needs_rmw_lock {
            Some(
                self.inner
                    .rmw_locks
                    .lock_for(&query.table, "__table_rmw_update__"),
            )
        } else {
            None
        };
        let _table_rmw_guard = table_rmw_lock.as_ref().map(|lock| lock.lock());
        let mut touched_ids: Vec<EntityId> = Vec::new();
        let claim_cap = query.claim_limit.map(|limit| limit as usize);
        let limit_cap = claim_cap.or_else(|| query.limit.map(|limit| limit as usize));
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
        let scan_limit = if query.order_by.is_empty() {
            limit_cap
        } else {
            None
        };
        let mut target_scan = super::dml_target_scan::DmlTargetScan::with_update_target(
            self,
            &query.table,
            effective_filter.as_ref(),
            scan_limit,
            query.target,
        );
        if needs_rmw_lock {
            target_scan = target_scan.with_live_table_rows();
        }
        let ids_to_update = target_scan.find_target_ids()?;
        let order_limit = if query.claim_limit.is_some() {
            None
        } else {
            limit_cap
        };
        let ids_to_update = if query.order_by.is_empty() {
            ids_to_update
        } else {
            ordered_update_target_ids(&manager, &ids_to_update, &query.order_by, order_limit)
        };
        let mut ids_to_update = if query.claim_limit.is_some() {
            self.filter_claim_locked_target_ids(&query.table, ids_to_update)
        } else {
            ids_to_update
        };
        if let Some(claim_cap) = claim_cap {
            ids_to_update.truncate(claim_cap);
        }
        if query.claim_exact
            && claim_cap.is_some_and(|claim_count| ids_to_update.len() < claim_count)
        {
            self.inner
                .claim_telemetry
                .record_miss(&query.table, claim_model);
            return Ok((
                RuntimeQueryResult::dml_result(raw_query.to_string(), 0, "update", "runtime-dml"),
                Vec::new(),
            ));
        }

        if needs_rmw_lock {
            let result = self.execute_update_inner_tracked_locked(
                raw_query,
                query,
                &compiled_plan,
                &ids_to_update,
                effective_filter.as_ref(),
            )?;
            if query.claim_limit.is_some() {
                self.record_pending_claim_locks_for_touched_ids(&query.table, &result.1);
            }
            record_claim_outcome(
                &self.inner.claim_telemetry,
                query.claim_limit,
                &query.table,
                claim_model,
                result.0.affected_rows,
            );
            return Ok(result);
        }

        let mut affected: u64 = 0;
        for chunk in ids_to_update.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            let mut applied_chunk = Vec::with_capacity(chunk.len());
            for entity in manager.get_many(chunk).into_iter().flatten() {
                let assignments =
                    self.materialize_update_assignments_for_entity(query, &entity, &compiled_plan)?;
                let applied = self.apply_materialized_update_for_entity(
                    query,
                    entity,
                    &compiled_plan,
                    assignments,
                )?;
                touched_ids.push(applied.id);
                applied_chunk.push(applied);
            }
            self.persist_update_chunk(&applied_chunk)?;
            affected += applied_chunk.len() as u64;
            let lsns = self.flush_update_chunk(&applied_chunk)?;
            if !query.suppress_events {
                self.emit_update_events_for_collection(&query.table, &applied_chunk, &lsns)?;
            }
        }

        if affected > 0 {
            self.note_table_write(&query.table);
        }
        if query.claim_limit.is_some() {
            self.record_pending_claim_locks_for_touched_ids(&query.table, &touched_ids);
        }
        record_claim_outcome(
            &self.inner.claim_telemetry,
            query.claim_limit,
            &query.table,
            claim_model,
            affected,
        );

        Ok((
            RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                affected,
                "update",
                "runtime-dml",
            ),
            touched_ids,
        ))
    }

    fn filter_claim_locked_target_ids(&self, table: &str, ids: Vec<EntityId>) -> Vec<EntityId> {
        let conn_id = current_connection_id();
        let locks = self.inner.pending_claim_locks.read();
        ids.into_iter()
            .filter(|id| {
                let Some(entity) = self.inner.db.store().get(table, *id) else {
                    return false;
                };
                let key = (table.to_string(), entity.logical_id());
                locks
                    .get(&key)
                    .is_none_or(|owner_conn_id| *owner_conn_id == conn_id)
            })
            .collect()
    }

    fn record_pending_claim_locks_for_touched_ids(&self, table: &str, ids: &[EntityId]) {
        if self.current_xid().is_none() || ids.is_empty() {
            return;
        }

        let conn_id = current_connection_id();
        let store = self.inner.db.store();
        let mut locks = self.inner.pending_claim_locks.write();
        for id in ids {
            let Some(entity) = store.get(table, *id) else {
                continue;
            };
            locks.insert((table.to_string(), entity.logical_id()), conn_id);
        }
    }

    fn execute_update_inner_tracked_locked(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
        compiled_plan: &CompiledUpdatePlan,
        ids_to_update: &[EntityId],
        effective_filter: Option<&Filter>,
    ) -> RedDBResult<(RuntimeQueryResult, Vec<EntityId>)> {
        let store = self.inner.db.store();
        let mut touched_ids = Vec::new();
        let mut lock_entries = Vec::new();

        for id in ids_to_update {
            let Some(candidate) = store.get(&query.table, *id) else {
                continue;
            };
            let logical_id = candidate.logical_id();
            let lock_key = format!("row:{}", logical_id.raw());
            let rmw_lock = self.inner.rmw_locks.lock_for(&query.table, &lock_key);
            lock_entries.push((lock_key, logical_id, rmw_lock));
        }

        lock_entries.sort_by(|left, right| left.0.cmp(&right.0));
        lock_entries.dedup_by(|left, right| left.0 == right.0);
        let _rmw_guards: Vec<_> = lock_entries.iter().map(|entry| entry.2.lock()).collect();

        let mut applied_chunk = Vec::new();
        for (_, logical_id, _) in &lock_entries {
            let Some(entity) = resolve_update_entity_by_logical_id(self, &query.table, *logical_id)
            else {
                continue;
            };
            if let Some(filter) = effective_filter {
                if !crate::runtime::query_exec::evaluate_entity_filter_with_db(
                    Some(self.inner.db.as_ref()),
                    &entity,
                    filter,
                    &query.table,
                    &query.table,
                ) {
                    continue;
                }
            }

            let assignments =
                self.materialize_update_assignments_for_entity(query, &entity, compiled_plan)?;
            let applied = self.apply_materialized_update_for_entity(
                query,
                entity,
                compiled_plan,
                assignments,
            )?;
            touched_ids.push(applied.id);
            applied_chunk.push(applied);
        }

        let affected = applied_chunk.len() as u64;
        if !applied_chunk.is_empty() {
            self.persist_update_chunk(&applied_chunk)?;
            let lsns = self.flush_update_chunk(&applied_chunk)?;
            if !query.suppress_events {
                self.emit_update_events_for_collection(&query.table, &applied_chunk, &lsns)?;
            }
        }

        if affected > 0 {
            self.note_table_write(&query.table);
        }

        Ok((
            RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                affected,
                "update",
                "runtime-dml",
            ),
            touched_ids,
        ))
    }

    fn compile_update_plan(&self, query: &UpdateQuery) -> RedDBResult<CompiledUpdatePlan> {
        let mut static_field_assignments = Vec::new();
        let mut static_metadata_assignments = Vec::new();
        let mut dynamic_assignments = Vec::new();
        let row_contract_plan = build_row_update_contract_plan(&self.db(), &query.table)?;
        let mut row_modified_columns = Vec::new();

        for (idx, (column, expr)) in query.assignment_exprs.iter().enumerate() {
            let compound_op = query.compound_assignment_ops.get(idx).copied().flatten();
            let metadata_key = resolve_sql_ttl_metadata_key(column);
            if compound_op.is_some() && metadata_key.is_some() {
                return Err(RedDBError::Query(format!(
                    "compound assignment is only supported for row fields: {column}"
                )));
            }
            if compound_op.is_none() {
                if let Ok(value) = fold_expr_to_value(expr.clone()) {
                    if let Some(metadata_key) = metadata_key {
                        let raw_value = sql_literal_to_metadata_value(metadata_key, &value)?;
                        let (canonical_key, canonical_value) =
                            canonicalize_sql_ttl_metadata(metadata_key, raw_value);
                        static_metadata_assignments
                            .push((canonical_key.to_string(), canonical_value));
                    } else {
                        let value = self.resolve_crypto_sentinel(value)?;
                        static_field_assignments.push((
                            column.clone(),
                            normalize_row_update_assignment_with_plan(
                                &query.table,
                                column,
                                value,
                                row_contract_plan.as_ref(),
                            )?,
                        ));
                        row_modified_columns.push(column.clone());
                    }
                    continue;
                }
            }

            dynamic_assignments.push(CompiledUpdateAssignment {
                column: column.clone(),
                expr: expr.clone(),
                compound_op,
                metadata_key,
                row_rule: if metadata_key.is_none() {
                    if let Some(plan) = row_contract_plan.as_ref() {
                        if plan.timestamps_enabled
                            && (column == "created_at" || column == "updated_at")
                        {
                            return Err(RedDBError::Query(format!(
                                "collection '{}' manages '{}' automatically — do not set it in UPDATE",
                                query.table, column
                            )));
                        }
                        if let Some(rule) = plan.declared_rules.get(column) {
                            Some(rule.clone())
                        } else if plan.strict_schema {
                            return Err(RedDBError::Query(format!(
                                "collection '{}' is strict and does not allow undeclared fields: {}",
                                query.table, column
                            )));
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                },
            });
            if metadata_key.is_none() {
                row_modified_columns.push(column.clone());
            }
        }

        let row_modified_columns = dedupe_update_columns(row_modified_columns);
        let row_touches_unique_columns = row_contract_plan.as_ref().is_some_and(|plan| {
            row_modified_columns.iter().any(|column| {
                plan.unique_columns
                    .keys()
                    .any(|unique| unique.eq_ignore_ascii_case(column))
            })
        });

        if let Some(ttl_ms) = query.ttl_ms {
            static_metadata_assignments
                .push(("_ttl_ms".to_string(), metadata_u64_to_value(ttl_ms)));
        }
        if let Some(expires_at_ms) = query.expires_at_ms {
            static_metadata_assignments.push((
                "_expires_at".to_string(),
                metadata_u64_to_value(expires_at_ms),
            ));
        }
        for (key, val) in &query.with_metadata {
            static_metadata_assignments.push((key.clone(), storage_value_to_metadata_value(val)));
        }

        Ok(CompiledUpdatePlan {
            static_field_assignments,
            static_metadata_assignments,
            dynamic_assignments,
            row_contract_plan,
            row_modified_columns,
            row_touches_unique_columns,
        })
    }

    fn materialize_update_assignments_for_entity(
        &self,
        query: &UpdateQuery,
        entity: &UnifiedEntity,
        compiled_plan: &CompiledUpdatePlan,
    ) -> RedDBResult<MaterializedUpdateAssignments> {
        let mut assignments = MaterializedUpdateAssignments::default();
        let mut record: Option<UnifiedRecord> = None;

        for assignment in &compiled_plan.dynamic_assignments {
            if assignment.compound_op.is_some()
                && !matches!(
                    entity.data,
                    EntityData::Row(_) | EntityData::Node(_) | EntityData::Edge(_)
                )
            {
                return Err(RedDBError::Query(format!(
                    "compound assignment is only supported for row or graph UPDATE column '{}'",
                    assignment.column
                )));
            }
            if record.is_none() {
                record = runtime_any_record_from_entity_ref(entity);
            }
            let Some(record) = record.as_ref() else {
                return Err(RedDBError::Query(format!(
                    "UPDATE could not materialize runtime record for entity {} in '{}'",
                    entity.id.raw(),
                    query.table
                )));
            };
            let rhs = super::expr_eval::evaluate_runtime_expr_with_db(
                Some(self.inner.db.as_ref()),
                &assignment.expr,
                record,
                Some(query.table.as_str()),
                Some(query.table.as_str()),
            )
            .ok_or_else(|| {
                RedDBError::Query(format!(
                    "failed to evaluate UPDATE expression for column '{}'",
                    assignment.column
                ))
            })?;
            let value = if let Some(op) = assignment.compound_op {
                evaluate_compound_update_assignment(&assignment.column, record, op, rhs)?
            } else {
                rhs
            };

            if let Some(metadata_key) = assignment.metadata_key {
                let raw_value = sql_literal_to_metadata_value(metadata_key, &value)?;
                let (canonical_key, canonical_value) =
                    canonicalize_sql_ttl_metadata(metadata_key, raw_value);
                assignments
                    .dynamic_metadata_assignments
                    .push((canonical_key.to_string(), canonical_value));
            } else {
                assignments.dynamic_field_assignments.push((
                    assignment.column.clone(),
                    normalize_row_update_value_for_rule(
                        &query.table,
                        self.resolve_crypto_sentinel(value)?,
                        assignment.row_rule.as_ref(),
                    )?,
                ));
            }
        }

        Ok(assignments)
    }

    fn apply_materialized_update_for_entity(
        &self,
        query: &UpdateQuery,
        entity: UnifiedEntity,
        compiled_plan: &CompiledUpdatePlan,
        mut assignments: MaterializedUpdateAssignments,
    ) -> RedDBResult<AppliedEntityMutation> {
        if matches!(query.target, UpdateTarget::Kv)
            && !compiled_plan
                .static_field_assignments
                .iter()
                .any(|(column, _)| column.eq_ignore_ascii_case("key"))
            && !assignments
                .dynamic_field_assignments
                .iter()
                .any(|(column, _)| column.eq_ignore_ascii_case("key"))
        {
            if let Some(key) = runtime_any_record_from_entity_ref(&entity)
                .and_then(|record| record.get("key").cloned())
            {
                assignments
                    .dynamic_field_assignments
                    .push(("key".to_string(), key));
            }
        }
        if matches!(entity.data, EntityData::Row(_)) {
            return self.apply_loaded_sql_update_row_core(
                query.table.clone(),
                entity,
                &compiled_plan.static_field_assignments,
                assignments.dynamic_field_assignments,
                &compiled_plan.static_metadata_assignments,
                assignments.dynamic_metadata_assignments,
                compiled_plan.row_contract_plan.as_ref(),
                &compiled_plan.row_modified_columns,
                compiled_plan.row_touches_unique_columns,
            );
        }

        ensure_graph_identity_update_allowed(&entity, compiled_plan, &assignments)?;

        let operations = build_patch_operations_from_materialized_assignments(
            &entity,
            compiled_plan,
            assignments,
        );
        self.apply_loaded_patch_entity_core(
            query.table.clone(),
            entity,
            crate::json::Value::Null,
            operations,
        )
    }

    /// Execute DELETE FROM table WHERE filter
    pub fn execute_delete(
        &self,
        raw_query: &str,
        query: &DeleteQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // Issue #523 — blockchain collections are immutable; see
        // execute_update for the same gate.
        if crate::runtime::blockchain_kind::is_chain(self.inner.db.store().as_ref(), &query.table) {
            return Err(RedDBError::InvalidOperation(format!(
                "BlockchainCollectionImmutable: DELETE not allowed on '{}'",
                query.table
            )));
        }
        // CollectionContract gate (#50) — see execute_update for
        // rationale. The gate handles APPEND ONLY rejection and is
        // the single point where future contract bits land.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Delete,
        )?;

        // RETURNING on DELETE: capture the pre-image via an internal
        // SELECT that reuses the same WHERE, then run the delete with
        // the RETURNING clause stripped, then project the captured
        // rows through the requested items. The extra SELECT is a
        // pragmatic MVP — a future pass can fuse the scan with the
        // delete to avoid the second pass over the heap.
        if let Some(items) = query.returning.clone() {
            let select_sql = delete_to_select_sql(raw_query).ok_or_else(|| {
                RedDBError::Query(
                    "DELETE ... RETURNING: cannot rewrite query for pre-image scan".to_string(),
                )
            })?;
            let captured = self.execute_query(&select_sql)?;

            let mut inner_query = query.clone();
            inner_query.returning = None;
            let _ = self.execute_delete(raw_query, &inner_query)?;

            let snapshots: Vec<Vec<(String, Value)>> = captured
                .result
                .records
                .iter()
                .map(|rec| {
                    rec.iter_fields()
                        .map(|(k, v)| (k.as_ref().to_string(), v.clone()))
                        .collect()
                })
                .collect();
            let affected = snapshots.len() as u64;
            let result = build_returning_result(&items, &snapshots, None);

            let mut response = RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                affected,
                "delete",
                "runtime-dml-returning",
            );
            response.result = result;
            return Ok(response);
        }
        // Row-Level Security enforcement (Phase 2.5.2 PG parity).
        //
        // When the table has RLS enabled, gate the DELETE by the
        // per-role policy set: mutations only touch rows that *every*
        // matching `FOR DELETE` policy would accept. No policies =>
        // zero rows affected (PG restrictive-default).
        if crate::runtime::impl_core::rls_is_enabled(self, &query.table) {
            let rls_filter = crate::runtime::impl_core::rls_policy_filter(
                self,
                &query.table,
                crate::storage::query::ast::PolicyAction::Delete,
            );
            let Some(policy) = rls_filter else {
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    0,
                    "delete",
                    "runtime-dml-rls",
                ));
            };
            // Fold the policy predicate into the user's WHERE before
            // dispatching — the remainder of this function reads the
            // filter from `query` via `effective_delete_filter`, which
            // respects the updated value.
            let mut augmented = query.clone();
            augmented.filter = Some(match augmented.filter.take() {
                Some(existing) => {
                    crate::storage::query::ast::Filter::And(Box::new(existing), Box::new(policy))
                }
                None => policy,
            });
            return self.execute_delete_inner(raw_query, &augmented);
        }
        self.execute_delete_inner(raw_query, query)
    }

    fn execute_delete_inner(
        &self,
        raw_query: &str,
        query: &DeleteQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let effective_filter = effective_delete_filter(query);

        // Find the rows that match the WHERE clause. The "find target
        // rows" loop lives in DmlTargetScan so UPDATE (#52) can reuse
        // the same scan strategy.
        let scan = super::dml_target_scan::DmlTargetScan::new(
            self,
            &query.table,
            effective_filter.as_ref(),
            None,
        );
        let ids_to_delete = scan.find_target_ids()?;

        // For event-enabled collections, snapshot the pre-delete state
        // before rows are physically removed.
        let needs_delete_events =
            !query.suppress_events && self.collection_has_delete_subscriptions(&query.table);
        let mut pre_images: HashMap<u64, crate::json::Value> = if needs_delete_events {
            scan.row_json_pre_images(&ids_to_delete)
        } else {
            HashMap::new()
        };

        let mut affected: u64 = 0;
        for chunk in ids_to_delete.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            let (count, lsns) = self.delete_entities_batch(&query.table, chunk)?;
            affected += count;
            if needs_delete_events && !lsns.is_empty() {
                // lsns.len() == actually-deleted entities; align with chunk ids.
                // `delete_batch` may skip missing entities, so we correlate by
                // the number returned (they're emitted in chunk order).
                let deleted_chunk = &chunk[..lsns.len().min(chunk.len())];
                self.emit_delete_events_for_collection(
                    &query.table,
                    deleted_chunk,
                    &lsns,
                    &pre_images,
                )?;
            }
        }
        pre_images.clear();

        if affected > 0 {
            self.note_table_write(&query.table);
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "delete",
            "runtime-dml",
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::schema::Value;
    use crate::storage::wal::{WalReader, WalRecord};
    use crate::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
    use crate::{RedDBOptions, RedDBRuntime};
    use std::path::Path;

    fn persistent_operational_options(path: &Path) -> RedDBOptions {
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageProfileSelection {
                deploy_profile: DeployProfile::Embedded,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            })
            .unwrap()
    }

    fn store_commit_batches(wal_path: &Path) -> Vec<Vec<Vec<u8>>> {
        WalReader::open(wal_path)
            .expect("wal opens")
            .iter()
            .map(|record| record.expect("wal record decodes").1)
            .filter_map(|record| match record {
                WalRecord::TxCommitBatch { actions, .. } => Some(actions),
                _ => None,
            })
            .collect()
    }

    fn action_contains_text(action: &[u8], needle: &str) -> bool {
        action
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    fn claim_metric_count(
        snapshot: &crate::runtime::ClaimTelemetrySnapshot,
        metric: &str,
        collection: &str,
        model: &str,
    ) -> u64 {
        let rows = match metric {
            "attempts" => &snapshot.attempts,
            "successful" => &snapshot.successful,
            "misses" => &snapshot.misses,
            "skipped_locked" => &snapshot.skipped_locked,
            other => panic!("unknown claim metric {other}"),
        };
        rows.iter()
            .find(|((actual_collection, actual_model), _)| {
                actual_collection == collection && actual_model == model
            })
            .map(|(_, count)| *count)
            .unwrap_or(0)
    }

    fn assert_statement_writes_collections_in_one_new_wal_batch(
        rt: &RedDBRuntime,
        wal_path: &Path,
        statement: &str,
        source: &str,
        event_queue: &str,
    ) {
        let before_batches = store_commit_batches(wal_path).len();

        rt.execute_query(statement).unwrap();

        let batches = store_commit_batches(wal_path);
        let statement_batches = &batches[before_batches..];
        let source_batch = statement_batches
            .iter()
            .position(|actions| {
                actions.iter().any(|action| {
                    action_contains_text(action, source)
                        && !action_contains_text(action, event_queue)
                })
            })
            .expect("source collection write batch is present");
        let event_batch = statement_batches
            .iter()
            .position(|actions| {
                actions
                    .iter()
                    .any(|action| action_contains_text(action, event_queue))
            })
            .expect("event queue write batch is present");

        assert_eq!(
            source_batch, event_batch,
            "WITH EVENTS must persist the source write and queue event in the same WAL batch"
        );
    }

    #[test]
    fn with_events_autocommit_persists_mutation_and_event_in_one_wal_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("events_dual_write.rdb");
        let wal_path = reddb_file::layout::unified_wal_path(&db_path);
        let rt = RedDBRuntime::with_options(persistent_operational_options(&db_path)).unwrap();

        rt.execute_query("CREATE TABLE users (id INT, email TEXT) WITH EVENTS")
            .unwrap();
        assert_statement_writes_collections_in_one_new_wal_batch(
            &rt,
            &wal_path,
            "INSERT INTO users (id, email) VALUES (1, 'a@example.test')",
            "users",
            "users_events",
        );
    }

    #[test]
    fn with_events_autocommit_update_persists_mutation_and_event_in_one_wal_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("events_update_atomic.rdb");
        let wal_path = reddb_file::layout::unified_wal_path(&db_path);
        let rt = RedDBRuntime::with_options(persistent_operational_options(&db_path)).unwrap();

        rt.execute_query(
            "CREATE TABLE users (id INT, email TEXT) WITH EVENTS (UPDATE) TO user_updates",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, email) VALUES (1, 'a@example.test')")
            .unwrap();

        assert_statement_writes_collections_in_one_new_wal_batch(
            &rt,
            &wal_path,
            "UPDATE users SET email = 'b@example.test' WHERE id = 1",
            "users",
            "user_updates",
        );
    }

    #[test]
    fn with_events_autocommit_delete_persists_mutation_and_event_in_one_wal_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("events_delete_atomic.rdb");
        let wal_path = reddb_file::layout::unified_wal_path(&db_path);
        let rt = RedDBRuntime::with_options(persistent_operational_options(&db_path)).unwrap();

        rt.execute_query(
            "CREATE TABLE users (id INT, email TEXT) WITH EVENTS (DELETE) TO user_deletes",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, email) VALUES (1, 'a@example.test')")
            .unwrap();

        assert_statement_writes_collections_in_one_new_wal_batch(
            &rt,
            &wal_path,
            "DELETE FROM users WHERE id = 1",
            "users",
            "user_deletes",
        );
    }

    #[test]
    fn update_where_id_in_with_hash_index_updates_expected_rows() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!("INSERT INTO users (id, score) VALUES ({id}, 0)"))
                .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_id ON users (id) USING HASH")
            .unwrap();

        let updated = rt
            .execute_query("UPDATE users SET score = 42 WHERE id IN (1,3,4)")
            .unwrap();
        assert_eq!(updated.affected_rows, 3);

        let selected = rt
            .execute_query("SELECT id, score FROM users ORDER BY id")
            .unwrap();
        let scores: Vec<(i64, i64)> = selected
            .result
            .records
            .iter()
            .map(|record| {
                let id = match record.get("id").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer id, got {other:?}"),
                };
                let score = match record.get("score").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer score, got {other:?}"),
                };
                (id, score)
            })
            .collect();
        assert_eq!(scores, vec![(0, 0), (1, 42), (2, 0), (3, 42), (4, 42)]);
    }

    /// Drives UPDATE through the shared `DmlTargetScan` module — the
    /// same code path DELETE uses (#51, #52). Exercises the indexed
    /// equality fast-path (WHERE id = N with a HASH index), the
    /// unindexed range scan (WHERE score > N), and the no-WHERE
    /// full-scan branch to confirm the extracted "find target rows"
    /// loop preserves affected-row counts and the resulting row state.
    #[test]
    fn update_routes_through_dml_target_scan_for_indexed_and_scan_paths() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!(
                "INSERT INTO items (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_items_id ON items (id) USING HASH")
            .unwrap();

        // Indexed equality UPDATE — hits the hash fast-path inside
        // DmlTargetScan::find_target_ids. id=2 has score=20, drop it
        // below the score>25 cutoff so the next assertion stays clean.
        let updated_one = rt
            .execute_query("UPDATE items SET score = 5 WHERE id = 2")
            .unwrap();
        assert_eq!(updated_one.affected_rows, 1);

        // Unindexed scan UPDATE — bumps everyone with score > 25,
        // i.e. ids 3 and 4 (scores 30, 40). Goes through the
        // zoned/full-scan branch.
        let updated_many = rt
            .execute_query("UPDATE items SET score = 7 WHERE score > 25")
            .unwrap();
        assert_eq!(updated_many.affected_rows, 2);

        let snapshot = rt
            .execute_query("SELECT id, score FROM items ORDER BY id")
            .unwrap();
        let pairs: Vec<(i64, i64)> = snapshot
            .result
            .records
            .iter()
            .map(|record| {
                let id = match record.get("id").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer id, got {other:?}"),
                };
                let score = match record.get("score").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer score, got {other:?}"),
                };
                (id, score)
            })
            .collect();
        assert_eq!(pairs, vec![(0, 0), (1, 10), (2, 5), (3, 7), (4, 7)]);

        // Full-scan UPDATE with no WHERE rewrites every remaining row.
        let updated_all = rt.execute_query("UPDATE items SET score = 1").unwrap();
        assert_eq!(updated_all.affected_rows, 5);
        let after = rt
            .execute_query("SELECT score FROM items ORDER BY id")
            .unwrap();
        let scores: Vec<i64> = after
            .result
            .records
            .iter()
            .map(|record| match record.get("score").unwrap() {
                Value::Integer(value) => *value,
                other => panic!("expected integer score, got {other:?}"),
            })
            .collect();
        assert_eq!(scores, vec![1, 1, 1, 1, 1]);
    }

    /// Drives DELETE through the new `DmlTargetScan` module. Exercises
    /// both the index fast-path (WHERE id = N with a HASH index) and
    /// the unindexed scan path (WHERE score > N) to confirm the
    /// extracted "find target rows" loop preserves the affected-row
    /// count and which rows survive.
    #[test]
    fn delete_routes_through_dml_target_scan_for_indexed_and_scan_paths() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!(
                "INSERT INTO items (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_items_id ON items (id) USING HASH")
            .unwrap();

        // Indexed equality DELETE — hits the hash fast-path inside
        // DmlTargetScan::find_target_ids.
        let deleted_one = rt.execute_query("DELETE FROM items WHERE id = 2").unwrap();
        assert_eq!(deleted_one.affected_rows, 1);

        // Unindexed scan DELETE — drops everyone with score > 25,
        // i.e. ids 3 and 4 (scores 30, 40). Goes through the
        // zoned/full-scan branch.
        let deleted_many = rt
            .execute_query("DELETE FROM items WHERE score > 25")
            .unwrap();
        assert_eq!(deleted_many.affected_rows, 2);

        let surviving = rt
            .execute_query("SELECT id FROM items ORDER BY id")
            .unwrap();
        let ids: Vec<i64> = surviving
            .result
            .records
            .iter()
            .map(|record| match record.get("id").unwrap() {
                Value::Integer(value) => *value,
                other => panic!("expected integer id, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![0, 1]);

        // Sanity: full-scan DELETE with no WHERE clears the rest.
        let deleted_rest = rt.execute_query("DELETE FROM items").unwrap();
        assert_eq!(deleted_rest.affected_rows, 2);
        let empty = rt.execute_query("SELECT id FROM items").unwrap();
        assert!(empty.result.records.is_empty());
    }

    /// CollectionContract gate (#49 + #50): APPEND ONLY tables accept
    /// INSERT but reject UPDATE and DELETE with the documented
    /// operator-facing error strings. Drives all three DML verbs so
    /// the centralized gate is exercised end-to-end.
    #[test]
    fn collection_contract_gate_blocks_update_and_delete_on_append_only() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE events (id INT, payload TEXT) APPEND ONLY")
            .unwrap();

        // INSERT must succeed — APPEND ONLY exists precisely to allow
        // appends. The gate should be a no-op for INSERT.
        let inserted = rt
            .execute_query("INSERT INTO events (id, payload) VALUES (1, 'hello')")
            .unwrap();
        assert_eq!(inserted.affected_rows, 1);

        // UPDATE is rejected with the gate's UPDATE-specific message.
        let update_err = rt
            .execute_query("UPDATE events SET payload = 'mut' WHERE id = 1")
            .unwrap_err();
        let msg = format!("{update_err}");
        assert!(
            msg.contains("APPEND ONLY") && msg.contains("UPDATE is rejected"),
            "expected UPDATE rejection message, got: {msg}"
        );

        // DELETE is rejected with the gate's DELETE-specific message.
        let delete_err = rt
            .execute_query("DELETE FROM events WHERE id = 1")
            .unwrap_err();
        let msg = format!("{delete_err}");
        assert!(
            msg.contains("APPEND ONLY") && msg.contains("DELETE is rejected"),
            "expected DELETE rejection message, got: {msg}"
        );

        // Row should still be present — neither rejected mutation
        // touched storage.
        let surviving = rt.execute_query("SELECT id FROM events").unwrap();
        assert_eq!(surviving.result.records.len(), 1);
    }

    /// CollectionContract gate: tables without an APPEND ONLY contract
    /// permit INSERT, UPDATE, and DELETE — the gate's default branch
    /// is a true pass-through, not an accidental block.
    #[test]
    fn collection_contract_gate_allows_all_verbs_on_unrestricted_table() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE notes (id INT, body TEXT)")
            .unwrap();

        rt.execute_query("INSERT INTO notes (id, body) VALUES (1, 'a')")
            .unwrap();
        let updated = rt
            .execute_query("UPDATE notes SET body = 'b' WHERE id = 1")
            .unwrap();
        assert_eq!(updated.affected_rows, 1);
        let deleted = rt.execute_query("DELETE FROM notes WHERE id = 1").unwrap();
        assert_eq!(deleted.affected_rows, 1);
    }

    #[test]
    fn insert_into_event_enabled_table_emits_event_to_configured_queue() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, email TEXT) WITH EVENTS (INSERT) TO audit_log",
        )
        .unwrap();

        let inserted = rt
            .execute_query("INSERT INTO users (id, email) VALUES (7, 'a@example.com')")
            .unwrap();
        assert_eq!(inserted.affected_rows, 1);

        let events = queue_payloads(&rt, "audit_log");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().expect("event payload object");
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("insert")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert_eq!(
            event.get("id").and_then(crate::json::Value::as_u64),
            Some(7)
        );
        assert!(event
            .get("ts")
            .and_then(crate::json::Value::as_u64)
            .is_some());
        assert!(event
            .get("lsn")
            .and_then(crate::json::Value::as_u64)
            .is_some());
        assert!(matches!(
            event.get("tenant"),
            Some(crate::json::Value::Null)
        ));
        assert!(matches!(
            event.get("before"),
            Some(crate::json::Value::Null)
        ));
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .expect("after object");
        assert_eq!(
            after.get("id").and_then(crate::json::Value::as_u64),
            Some(7)
        );
        assert_eq!(
            after.get("email").and_then(crate::json::Value::as_str),
            Some("a@example.com")
        );
    }

    #[test]
    fn multi_row_insert_emits_one_insert_event_per_row_in_order() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, email TEXT) WITH EVENTS")
            .unwrap();

        rt.execute_query(
            "INSERT INTO users (id, email) VALUES (1, 'a@example.com'), (2, 'b@example.com')",
        )
        .unwrap();

        let events = queue_payloads(&rt, "users_events");
        assert_eq!(events.len(), 2);
        let mut previous_lsn = 0;
        for (event, expected_id) in events.iter().zip([1_u64, 2]) {
            let object = event.as_object().expect("event payload object");
            assert_eq!(
                object.get("op").and_then(crate::json::Value::as_str),
                Some("insert")
            );
            assert_eq!(
                object.get("id").and_then(crate::json::Value::as_u64),
                Some(expected_id)
            );
            let lsn = object
                .get("lsn")
                .and_then(crate::json::Value::as_u64)
                .expect("event lsn");
            assert!(
                lsn > previous_lsn,
                "event LSNs should increase in row order"
            );
            previous_lsn = lsn;
            let after = object
                .get("after")
                .and_then(crate::json::Value::as_object)
                .expect("after object");
            assert_eq!(
                after.get("id").and_then(crate::json::Value::as_u64),
                Some(expected_id)
            );
        }
    }

    #[test]
    fn claim_metrics_increment_skipped_locked_without_counting_miss() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        rt.execute_query("CREATE TABLE claim_metric_locked (id INT, rank INT, status TEXT)")
            .expect("create table");
        // ADR 0063: index-backed claim ordering on `rank`.
        rt.execute_query("CREATE INDEX idx_claim_metric_locked_rank ON claim_metric_locked (rank)")
            .expect("create index");
        rt.execute_query(
            "INSERT INTO claim_metric_locked (id, rank, status) VALUES \
             (1, 10, 'ready'), (2, 20, 'ready')",
        )
        .expect("insert rows");

        let claim_lock = rt
            .inner
            .rmw_locks
            .lock_for("claim_metric_locked", "__table_claim_update__");
        let _guard = claim_lock.lock();
        let updated = rt
            .execute_query(
                "UPDATE claim_metric_locked SET status = 'claimed' WHERE status = 'ready' \
                 CLAIM LIMIT 2 ORDER BY rank ASC",
            )
            .expect("claim skips locked candidates");

        assert_eq!(updated.affected_rows, 0);
        let snapshot = rt.claim_telemetry_snapshot();
        assert_eq!(
            claim_metric_count(&snapshot, "attempts", "claim_metric_locked", "table"),
            1
        );
        assert_eq!(
            claim_metric_count(&snapshot, "skipped_locked", "claim_metric_locked", "table"),
            2
        );
        assert_eq!(
            claim_metric_count(&snapshot, "misses", "claim_metric_locked", "table"),
            0
        );
        assert_eq!(
            snapshot.skipped_locked.len(),
            1,
            "skipped-lock labels stay bounded to collection/model"
        );
    }

    fn queue_payloads(rt: &RedDBRuntime, queue: &str) -> Vec<crate::json::Value> {
        let result = rt
            .execute_query(&format!("QUEUE PEEK {queue} 10"))
            .expect("peek queue");
        result
            .result
            .records
            .iter()
            .map(
                |record| match record.get("payload").expect("payload column") {
                    Value::Json(bytes) => crate::json::from_slice(bytes).expect("json payload"),
                    other => panic!("expected JSON queue payload, got {other:?}"),
                },
            )
            .collect()
    }

    // ── #112: auto-index user `id` on first insert ─────────────────────

    /// First insert into a fresh collection that carries a column named
    /// `id` registers an implicit HASH index on `id`. Subsequent inserts
    /// populate it transparently, and `WHERE id = N` lookups exercise
    /// the hash-index fast path in `DmlTargetScan::find_target_ids`.
    ///
    /// This is the load-bearing acceptance test for #112 — without the
    /// hook, `find_index_for_column` returns `None` and DELETE/UPDATE
    /// fall through to a full segment scan (the 4× perf gap documented
    /// in `docs/perf/delete-sequential-2026-05-06.md`).
    #[test]
    fn auto_index_id_fires_on_first_insert() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE bench_users (id INT, score INT)")
            .unwrap();

        // Pre-condition: no index on `id` yet.
        assert!(
            rt.index_store_ref()
                .find_index_for_column("bench_users", "id")
                .is_none(),
            "freshly created collection should not have an `id` index"
        );

        // Single-row INSERT — drives `MutationEngine::append_one`.
        rt.execute_query("INSERT INTO bench_users (id, score) VALUES (1, 10)")
            .unwrap();

        // Post-condition: hash index registered on `id`.
        let registered = rt
            .index_store_ref()
            .find_index_for_column("bench_users", "id")
            .expect("auto-index hook should have registered idx_id on first insert");
        assert_eq!(registered.name, "idx_id");
        assert_eq!(registered.collection, "bench_users");
        assert_eq!(registered.columns, vec!["id".to_string()]);
        assert!(matches!(
            registered.method,
            super::super::index_store::IndexMethodKind::Hash
        ));

        // Subsequent inserts populate the index; `WHERE id = N` should
        // resolve via the hash fast path and round-trip every row.
        for id in 2..=5 {
            rt.execute_query(&format!(
                "INSERT INTO bench_users (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        for id in 1..=5 {
            let result = rt
                .execute_query(&format!("SELECT score FROM bench_users WHERE id = {id}"))
                .unwrap();
            assert_eq!(
                result.result.records.len(),
                1,
                "id={id} should match one row"
            );
        }

        // Delete via the hash fast-path — exactly the bench scenario the
        // perf doc identified as the 4× regression. With the index
        // present, `find_target_ids` short-circuits before
        // `for_each_entity_zoned` runs.
        let deleted = rt
            .execute_query("DELETE FROM bench_users WHERE id = 3")
            .unwrap();
        assert_eq!(deleted.affected_rows, 1);
    }

    /// Bulk INSERT (the multi-row VALUES path) drives
    /// `MutationEngine::append_batch`. The hook must fire there too —
    /// otherwise the batch entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk INSERT) skip auto-indexing entirely.
    #[test]
    fn auto_index_id_fires_on_first_bulk_insert() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE bench_bulk (id INT, score INT)")
            .unwrap();

        rt.execute_query("INSERT INTO bench_bulk (id, score) VALUES (1, 10), (2, 20), (3, 30)")
            .unwrap();

        let registered = rt
            .index_store_ref()
            .find_index_for_column("bench_bulk", "id")
            .expect("auto-index hook should fire on first bulk insert");
        assert_eq!(registered.name, "idx_id");

        // Every row populated via `index_entity_insert_batch`.
        for id in 1..=3 {
            let result = rt
                .execute_query(&format!("SELECT score FROM bench_bulk WHERE id = {id}"))
                .unwrap();
            assert_eq!(result.result.records.len(), 1);
        }
    }

    /// Hook is a no-op when the row carries no `id` column. Conservative
    /// match (case-sensitive `id`) — `Id`, `ID`, and `rid`
    /// don't trigger it.
    #[test]
    fn auto_index_id_skips_when_no_id_column() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE plain (uid INT, label TEXT)")
            .unwrap();
        rt.execute_query("INSERT INTO plain (uid, label) VALUES (1, 'a')")
            .unwrap();

        assert!(rt
            .index_store_ref()
            .find_index_for_column("plain", "id")
            .is_none());
        assert!(rt
            .index_store_ref()
            .find_index_for_column("plain", "uid")
            .is_none());
    }

    /// Hook only fires once per collection. If an explicit
    /// `CREATE INDEX ... USING BTREE` already covers `id`, the hook
    /// detects it via `find_index_for_column` and does NOT clobber it
    /// with a HASH index on the next insert.
    #[test]
    fn auto_index_id_skips_when_index_already_exists() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE pre (id INT, score INT)")
            .unwrap();
        // User-declared BTREE index on `id` before any insert.
        rt.execute_query("CREATE INDEX user_idx ON pre (id) USING BTREE")
            .unwrap();
        rt.execute_query("INSERT INTO pre (id, score) VALUES (1, 10)")
            .unwrap();

        let registered = rt
            .index_store_ref()
            .find_index_for_column("pre", "id")
            .expect("user index should still be there");
        assert_eq!(
            registered.name, "user_idx",
            "auto-index hook must not overwrite an existing index"
        );
    }

    /// Implicit `idx_id` is reaped when the collection drops. The
    /// existing `execute_drop_table` walks `list_indices` and drops every
    /// entry — confirm the auto-created index participates.
    #[test]
    fn auto_index_id_dropped_with_collection() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE ephemeral (id INT, score INT)")
            .unwrap();
        rt.execute_query("INSERT INTO ephemeral (id, score) VALUES (1, 10)")
            .unwrap();
        assert!(rt
            .index_store_ref()
            .find_index_for_column("ephemeral", "id")
            .is_some());

        rt.execute_query("DROP TABLE ephemeral").unwrap();

        assert!(
            rt.index_store_ref()
                .find_index_for_column("ephemeral", "id")
                .is_none(),
            "implicit `idx_id` must be reaped when its collection drops"
        );
    }

    /// Opt-out via `RedDBOptions::with_auto_index_id(false)` (which
    /// forwards to `UnifiedStoreConfig::auto_index_id`). With the knob
    /// off, first insert leaves the collection without an `id` index —
    /// DELETE/UPDATE fall back to the scan path.
    #[test]
    fn auto_index_id_disabled_by_config() {
        let opts = RedDBOptions::in_memory().with_auto_index_id(false);
        let rt = RedDBRuntime::with_options(opts).unwrap();

        rt.execute_query("CREATE TABLE off (id INT, score INT)")
            .unwrap();
        rt.execute_query("INSERT INTO off (id, score) VALUES (1, 10)")
            .unwrap();

        assert!(
            rt.index_store_ref()
                .find_index_for_column("off", "id")
                .is_none(),
            "with auto_index_id=false, no implicit index should be created"
        );
    }

    // ── #293: UPDATE / DELETE events ─────────────────────────────────────

    #[test]
    fn update_single_row_emits_update_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT) WITH EVENTS (UPDATE) TO audit_log",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1")
            .unwrap();

        let events = queue_payloads(&rt, "audit_log");
        assert_eq!(events.len(), 1, "expected exactly 1 update event");
        let event = events[0].as_object().expect("event payload object");
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("update")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|v| !v.is_empty()));
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .expect("before must be an object");
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .expect("after must be an object");
        assert_eq!(
            before.get("name").and_then(crate::json::Value::as_str),
            Some("Alice"),
            "before.name should be the old value"
        );
        assert_eq!(
            after.get("name").and_then(crate::json::Value::as_str),
            Some("Bob"),
            "after.name should be the new value"
        );
    }

    #[test]
    fn update_event_only_includes_changed_fields() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT, email TEXT) WITH EVENTS (UPDATE) TO evts",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'a@x.com')")
            .unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().unwrap();
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        // Only changed field included.
        assert!(
            before.contains_key("name"),
            "before must include changed field"
        );
        assert!(
            after.contains_key("name"),
            "after must include changed field"
        );
        // Unchanged fields must not appear.
        assert!(
            !before.contains_key("email"),
            "before must not include unchanged email"
        );
        assert!(
            !after.contains_key("email"),
            "after must not include unchanged email"
        );
    }

    #[test]
    fn multi_row_update_emits_one_event_per_row() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, status TEXT) WITH EVENTS (UPDATE) TO evts")
            .unwrap();
        rt.execute_query(
            "INSERT INTO items (id, status) VALUES (1, 'new'), (2, 'new'), (3, 'new')",
        )
        .unwrap();

        rt.execute_query("UPDATE items SET status = 'done'")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(events.len(), 3, "expected one update event per row");
        for event in &events {
            let obj = event.as_object().unwrap();
            assert_eq!(
                obj.get("op").and_then(crate::json::Value::as_str),
                Some("update")
            );
        }
    }

    #[test]
    fn delete_single_row_emits_delete_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS (DELETE) TO del_log")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (42, 'Alice')")
            .unwrap();

        rt.execute_query("DELETE FROM users WHERE id = 42").unwrap();

        let events = queue_payloads(&rt, "del_log");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().expect("event payload object");
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("delete")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|v| !v.is_empty()));
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .expect("before must be an object for delete");
        assert_eq!(
            before.get("id").and_then(crate::json::Value::as_u64),
            Some(42)
        );
        assert_eq!(
            before.get("name").and_then(crate::json::Value::as_str),
            Some("Alice")
        );
        assert!(matches!(event.get("after"), Some(crate::json::Value::Null)));
    }

    #[test]
    fn multi_row_delete_emits_one_event_per_row() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, val INT) WITH EVENTS (DELETE) TO del_log")
            .unwrap();
        rt.execute_query("INSERT INTO items (id, val) VALUES (1, 10), (2, 20), (3, 30)")
            .unwrap();

        rt.execute_query("DELETE FROM items").unwrap();

        let events = queue_payloads(&rt, "del_log");
        assert_eq!(events.len(), 3, "expected one delete event per deleted row");
        for event in &events {
            let obj = event.as_object().unwrap();
            assert_eq!(
                obj.get("op").and_then(crate::json::Value::as_str),
                Some("delete")
            );
            assert!(matches!(obj.get("after"), Some(crate::json::Value::Null)));
        }
    }

    #[test]
    fn ops_filter_update_does_not_emit_on_insert_or_delete() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS (UPDATE) TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();
        rt.execute_query("DELETE FROM users WHERE id = 1").unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "UPDATE-only filter must not emit INSERT or DELETE events"
        );
    }

    // ── SUPPRESS EVENTS ────────────────────────────────────────────────────

    #[test]
    fn suppress_events_on_insert_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent INSERT events"
        );
    }

    #[test]
    fn suppress_events_on_update_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();
        // drain the INSERT event
        let _ = queue_payloads(&rt, "evts");
        // Force pop to drain; simpler: just check new count after UPDATE
        rt.execute_query("QUEUE PURGE evts").unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1 SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent UPDATE events"
        );
    }

    #[test]
    fn suppress_events_on_delete_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT) WITH EVENTS (INSERT, DELETE) TO evts",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();

        rt.execute_query("DELETE FROM users WHERE id = 1 SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent DELETE events"
        );
    }

    #[test]
    fn normal_insert_after_suppress_still_emits() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (2, 'Bob')")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(
            events.len(),
            1,
            "only the non-suppressed INSERT should emit"
        );
        assert_eq!(
            events[0].get("id").and_then(crate::json::Value::as_u64),
            Some(2)
        );
    }
}
