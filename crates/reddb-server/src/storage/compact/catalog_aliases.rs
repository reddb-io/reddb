//! Reconcile known runtime metadata on an isolated canonical-config recovery.
//! No arbitrary collection, queue delivery state or conflicting definition is discarded.
use super::*;
use crate::storage::wal::{WalReader, WalRecord};
use crate::storage::{unified::UnifiedStore, EntityId, EntityKind, UnifiedEntity};
use reddb_types::Value;

const PAIRS: &[(&str, &str)] = &[
    ("red_stats", "red.query.stats_catalog"),
    ("red_index_registry", "red.index.registry"),
    ("red_queue_meta", "red.queue.meta"),
    (
        "red_materialized_view_defs",
        "red.materialized_view.definitions",
    ),
];
const READ_BUDGET: usize = 4 * 1024 * 1024;
const PROOF_BUDGET: usize = 64 * 1024 * 1024;

type Fields = BTreeMap<String, Value>;
struct Row {
    entity: UnifiedEntity,
    fields: Fields,
}
struct Alias {
    legacy: &'static str,
    canonical: &'static str,
    entity_next: u64,
    row_next: u64,
    sequence_next: u64,
    missing: Vec<UnifiedEntity>,
}
#[derive(Default)]
pub(crate) struct CanonicalCatalogPlan {
    aliases: Vec<Alias>,
}

fn text_field(fields: &Fields, name: &str) -> Result<String> {
    match fields.get(name) {
        Some(Value::Text(value)) => Ok(value.to_string()),
        _ => Err(format!("canonical catalog proof requires text field {name}").into()),
    }
}
fn shape(legacy: &str, fields: &Fields) -> Result<Vec<String>> {
    let kind = fields.get("kind").and_then(|v| {
        if let Value::Text(s) = v {
            Some(s.as_ref())
        } else {
            None
        }
    });
    let (allowed, keys): (&[&str], &[&str]) = match legacy {
        "red_stats" if kind == Some("table") => (
            &["kind", "table", "row_count", "avg_row_size", "page_count"],
            &["kind", "table"],
        ),
        "red_stats" if kind == Some("column") => (
            &[
                "kind",
                "table",
                "column",
                "distinct_count",
                "null_count",
                "min_value",
                "max_value",
                "hist_total_count",
                "hist_bounds",
                "mcv_values",
                "mcv_freqs",
            ],
            &["kind", "table", "column"],
        ),
        "red_index_registry" if fields.get("dropped") == Some(&Value::Boolean(true)) => {
            (&["collection", "name", "dropped"], &["collection", "name"])
        }
        "red_index_registry" if fields.get("dropped") == Some(&Value::Boolean(false)) => (
            &[
                "collection",
                "name",
                "columns",
                "method",
                "resolution",
                "unique",
                "dropped",
            ],
            &["collection", "name"],
        ),
        "red_queue_meta" if kind == Some("queue_group") => (
            &["kind", "queue", "group", "created_at_ns"],
            &["kind", "queue", "group"],
        ),
        "red_materialized_view_defs" => (
            &[
                "name",
                "refresh_every_ms",
                "retention_duration_ms",
                "source_collections",
                "source_sql",
            ],
            &["name"],
        ),
        _ => return Err(format!("unsupported legacy metadata in {legacy}").into()),
    };
    ensure(
        fields.len() == allowed.len() && allowed.iter().all(|k| fields.contains_key(*k)),
        "unrecognized runtime metadata fields",
    )?;
    keys.iter().map(|key| text_field(fields, key)).collect()
}

impl CanonicalCatalogPlan {
    pub(super) fn prepare(path: &Path, wal: &Path) -> Result<Self> {
        let source = Arc::new(Source::open_for_wal_recovery(path)?);
        ensure(
            !source.pending_double_write && !source.header.checkpoint_in_progress,
            "catalog proof requires a recovered checkpoint",
        )?;
        let catalog = Catalog::read(&source)?;
        let Some(resume) = catalog.resume.as_ref() else {
            return Ok(Self::default());
        };
        let mut plan = Self::default();
        let mut bytes_read = 0usize;
        for &(legacy, canonical) in PAIRS {
            let (Some(a), Some(b)) = (
                resume.collections.get(legacy),
                resume.collections.get(canonical),
            ) else {
                continue;
            };
            if a.entities == 0 || b.entities == 0 || a.root_page == b.root_page {
                continue;
            }
            for (name, state) in [(legacy, a), (canonical, b)] {
                ensure(
                    catalog
                        .roots
                        .iter()
                        .filter(|r| r.collection == name)
                        .count()
                        == 1
                        && catalog
                            .roots
                            .iter()
                            .any(|r| r.collection == name && r.root_page == state.root_page),
                    "runtime catalog root differs from resume identity",
                )?;
            }
            catalog.verify_alias_has_no_references(legacy)?;
            let mut floors = (a.entity_id_next, a.row_id_next, a.sequence_id_next);
            let old = read_rows(
                &source,
                a.root_page,
                catalog.header.format_version,
                legacy,
                canonical,
                false,
                &mut bytes_read,
                &mut floors,
            )?;
            let current = read_rows(
                &source,
                b.root_page,
                catalog.header.format_version,
                legacy,
                canonical,
                true,
                &mut bytes_read,
                &mut floors,
            )?;
            let mut missing = Vec::new();
            for (key, row) in old {
                if legacy == "red_stats" {
                    continue;
                } // Recomputed planner observations, not definitions.
                if let Some(other) = current.get(&key) {
                    let mut previous = row.fields;
                    let mut selected = other.fields.clone();
                    if legacy == "red_queue_meta" {
                        // Group identity is (queue, group). Its creation observation
                        // is not delivery state; pending/ack/config rows are refused.
                        previous.remove("created_at_ns");
                        selected.remove("created_at_ns");
                    }
                    ensure(
                        previous == selected,
                        &format!("conflicting canonical definition in {legacy}"),
                    )?;
                } else {
                    missing.push(row.entity);
                }
            }
            plan.aliases.push(Alias {
                legacy,
                canonical,
                entity_next: floors.0,
                row_next: floors.1,
                sequence_next: floors.2,
                missing,
            });
        }
        if !plan.aliases.is_empty() && wal_has_records(wal)? {
            for entry in WalReader::open(wal)?.iter() {
                match entry?.1 {
                    WalRecord::TxCommitBatch { actions, .. } => {
                        for action in actions {
                            plan.verify_action(&action)?;
                        }
                    }
                    WalRecord::PageWrite { data, .. } => plan.verify_action(&data)?,
                    WalRecord::VectorInsert { collection, .. }
                    | WalRecord::ProbabilisticDelta {
                        name: collection, ..
                    } => ensure(
                        !plan.owns(&collection),
                        "WAL changes metadata selected for catalog recovery",
                    )?,
                    WalRecord::FullPageImage { .. } => {
                        return Err("catalog proof cannot precede physical undo".into())
                    }
                    _ => {}
                }
            }
        }
        Ok(plan)
    }
    fn owns(&self, name: &str) -> bool {
        self.aliases
            .iter()
            .any(|a| a.legacy == name || a.canonical == name)
    }
    fn verify_action(&self, bytes: &[u8]) -> Result<()> {
        use f::StoreWalActionFrame as A;
        let action = f::decode_store_wal_action_frame(bytes)?;
        let name = match &action {
            A::CreateCollection { name } | A::DropCollection { name } => name,
            A::UpsertEntityRecord { collection, .. }
            | A::DeleteEntityRecord { collection, .. }
            | A::BulkUpsertEntityRecords { collection, .. }
            | A::RefreshCollection { collection, .. } => collection,
            A::SetAuxMetadata { .. } => {
                return Err("auxiliary WAL requires independent catalog recovery".into())
            }
        };
        ensure(
            !self.owns(name),
            "WAL changes metadata selected for catalog recovery",
        )
    }
    pub(crate) fn retires(&self, legacy: &str, canonical: &str) -> bool {
        self.aliases
            .iter()
            .any(|a| a.legacy == legacy && a.canonical == canonical)
    }
    pub(super) fn reconciled_count(&self) -> usize {
        self.aliases.len()
    }
    pub(super) fn imported_count(&self) -> usize {
        self.aliases.iter().map(|a| a.missing.len()).sum()
    }

    pub(super) fn restore(&self, store: &UnifiedStore) -> Result<()> {
        // Reserve every retired identity before importing any definition.
        // An old row_id may already belong to another canonical descriptor.
        for alias in &self.aliases {
            let manager = store
                .get_collection(alias.canonical)
                .ok_or("canonical metadata disappeared")?;
            manager.restore_identity_counters(alias.entity_next, alias.row_next);
            manager.restore_sequence_id_next(alias.sequence_next);
            store.register_entity_id(EntityId::new(alias.entity_next.saturating_sub(1)));
        }
        for alias in &self.aliases {
            for previous in &alias.missing {
                let mut entity = UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::TableRow {
                        table: Arc::from(alias.canonical),
                        row_id: 0,
                    },
                    previous.data.clone(),
                );
                entity.created_at = previous.created_at;
                entity.updated_at = previous.updated_at;
                store.insert_auto(alias.canonical, entity)?;
            }
        }
        Ok(())
    }
}

fn read_rows(
    source: &Arc<Source>,
    root: u32,
    version: u32,
    legacy: &str,
    canonical: &str,
    is_canonical: bool,
    bytes_read: &mut usize,
    floors: &mut (u64, u64, u64),
) -> Result<BTreeMap<Vec<String>, Row>> {
    let tree = source.tree(root, true)?;
    let mut cursor = tree.cursor_first()?;
    let mut rows = BTreeMap::<Vec<String>, Row>::new();
    while let Some((key, bytes)) = cursor.next_with_budget(READ_BUDGET)? {
        *bytes_read = bytes_read
            .checked_add(bytes.len())
            .ok_or("catalog proof budget overflow")?;
        ensure(
            *bytes_read <= PROOF_BUDGET,
            "catalog proof exceeds memory budget",
        )?;
        let (entity, metadata) =
            reddb_engine::storage::metadata_codec::deserialize_entity_record(&bytes, version)?;
        ensure(
            metadata.is_none()
                && entity.cross_refs().is_empty()
                && entity.embeddings().is_empty()
                && entity.xmin == 0
                && entity.xmax == 0,
            "runtime metadata has nontrivial references, metadata or MVCC state",
        )?;
        let EntityKind::TableRow { table, row_id } = &entity.kind else {
            return Err("runtime metadata is not a row".into());
        };
        ensure(
            matches!(table.as_ref(),s if s==legacy||s==canonical)
                && key.as_slice() == entity.id.raw().to_be_bytes(),
            "runtime metadata identity mismatch",
        )?;
        floors.0 = floors.0.max(
            entity
                .id
                .raw()
                .max(entity.logical_id().raw())
                .checked_add(1)
                .ok_or("metadata IDs exhausted")?,
        );
        floors.1 = floors
            .1
            .max(row_id.checked_add(1).ok_or("metadata row IDs exhausted")?);
        floors.2 = floors.2.max(
            entity
                .sequence_id
                .checked_add(1)
                .ok_or("metadata sequences exhausted")?,
        );
        let fields = entity
            .data
            .as_row()
            .ok_or("missing runtime metadata fields")?
            .iter_fields()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect::<Fields>();
        // Canonical queue config and delivery rows remain intact. Only legacy
        // group declarations are eligible for reconciliation.
        if is_canonical
            && legacy == "red_queue_meta"
            && fields.get("kind") != Some(&Value::text("queue_group"))
        {
            continue;
        }
        let key = shape(legacy, &fields)?;
        if rows
            .get(&key)
            .is_none_or(|previous| entity.id.raw() > previous.entity.id.raw())
        {
            rows.insert(key, Row { entity, fields });
        }
    }
    Ok(rows)
}
