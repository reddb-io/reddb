//! Per-model collection view `red.*` snapshot builders.
//!
//! Extracted from the `red_schema` dispatcher (issue #1639). Serves
//! `red.tables`, `red.documents`, `red.kv`, `red.vectors`, `red.graphs`,
//! `red.timeseries`, `red.metrics`, `red.hypertable_chunks`, and
//! `red.timeseries_writes`.

use super::helpers::*;
use super::*;

/// Issue #745 — typed `red.tables` projection. Model-shaped view over
/// `red.collections` filtered to `model = table`, joined with the
/// declared column contract for `has_primary_key` and `column_count`.
pub(super) fn tables_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        TABLE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Table)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            let column_count = contract
                .as_ref()
                .map(|c| c.declared_columns.len() as u64)
                .unwrap_or(0);
            let has_primary_key = contract
                .as_ref()
                .map(|c| c.declared_columns.iter().any(|col| col.primary_key))
                .unwrap_or(false);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(column_count),
                    Value::UnsignedInteger(collection.indices.len() as u64),
                    Value::Boolean(has_primary_key),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
/// Issue #745 — typed `red.documents` projection. Filtered to
/// `model = document`. `inferred_field_count` reuses the same
/// inference path that `red.columns` uses for document collections,
/// so the two surfaces cannot drift.
pub(super) fn documents_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        DOCUMENT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Document)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let (document_count, inferred_field_count) = document_counts(runtime, &collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(document_count),
                    Value::UnsignedInteger(inferred_field_count),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
/// Issue #745 — typed `red.kv` projection. Filtered to
/// `model = kv`. `supports_prefix_scan` is a stable capability
/// indicator (true — KV always supports `KEYS WITH PREFIX`). The
/// declared key/value shape is reported as text-keyed with a
/// mixed-value hint when no declared contract pins it down.
pub(super) fn kv_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        KV_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Kv)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            // KV defaults to text keys. Value shape can be pinned by a
            // declared `value` column; otherwise it's `mixed` (any
            // value type is accepted).
            let key_type = contract
                .as_ref()
                .and_then(|c| {
                    c.declared_columns
                        .iter()
                        .find(|col| col.name == "key")
                        .map(|col| {
                            col.sql_type
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| col.data_type.clone())
                        })
                })
                .unwrap_or_else(|| "TEXT".to_string());
            let value_type = contract
                .as_ref()
                .and_then(|c| {
                    c.declared_columns
                        .iter()
                        .find(|col| col.name == "value")
                        .map(|col| {
                            col.sql_type
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| col.data_type.clone())
                        })
                })
                .unwrap_or_else(|| "mixed".to_string());
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::text(key_type),
                    Value::text(value_type),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
/// Issue #746 — typed `red.vectors` projection. Filtered to
/// `model = vector`. `dimensions` / `metric` come from the catalog
/// contract — both are NULL when undeclared (e.g. dynamic-mode
/// vector collection). `artifact_state` / `search_capable` come
/// from the vector introspection registry (#743). The registry is
/// populated lazily by the engine; when there is no published row
/// for this collection, `artifact_state` defaults to `unavailable`
/// and `search_capable` defaults to `false`, per the thread-
/// discussion decision on #746 (stable explicit values, not NULL).
pub(super) fn vectors_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        VECTOR_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Vector)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            let dimensions = collection
                .vector_dimension
                .map(|d| Value::UnsignedInteger(d as u64))
                .unwrap_or(Value::Null);
            let metric = collection
                .vector_metric
                .map(|m| Value::text(distance_metric_name(m)))
                .unwrap_or(Value::Null);
            let introspection = runtime.vector_introspection_get(&collection.name);
            let (artifact_state, search_capable) = match introspection {
                Some(row) => (row.artifact.state.as_str(), row.vector.search_capable),
                None => ("unavailable", false),
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    dimensions,
                    metric,
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::Boolean(search_capable),
                    Value::text(artifact_state),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
/// Issue #747 — typed `red.timeseries` projection. Filtered to
/// `model = time_series`. When the underlying collection was created
/// via `CREATE HYPERTABLE`, chunk-derived columns are populated from
/// the live `HypertableRegistry`; standalone timeseries report
/// `is_hypertable = false` and `NULL` for those columns.
pub(super) fn timeseries_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        TIMESERIES_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let registry = db.hypertables();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| {
            // `model = time_series` covers plain `CREATE TIMESERIES`;
            // `CREATE HYPERTABLE` declares a Table contract but
            // registers a hypertable spec — pick those up too so the
            // chart UI doesn't have to query two surfaces.
            c.model == CollectionModel::TimeSeries || registry.get(&c.name).is_some()
        })
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let spec = registry.get(&collection.name);
            let chunks = if spec.is_some() {
                registry.show_chunks(&collection.name)
            } else {
                Vec::new()
            };
            let is_hypertable = spec.is_some();
            let time_column = spec.as_ref().map(|s| s.time_column.clone());
            // Chunk widths are nanoseconds in the registry; expose
            // milliseconds so the UI doesn't have to convert.
            let chunk_interval_ms = spec.as_ref().map(|s| s.chunk_interval_ns / 1_000_000);
            let chunk_count = chunks.len() as u64;
            let (oldest_ns, newest_ns) =
                chunks
                    .iter()
                    .fold((None::<u64>, None::<u64>), |(oldest, newest), chunk| {
                        // Empty chunks have `min_ts_ns = u64::MAX`;
                        // skip those when computing the overall min so
                        // an empty hypertable shows `NULL` rather than
                        // `u64::MAX`.
                        let next_oldest = if chunk.row_count == 0 {
                            oldest
                        } else {
                            Some(match oldest {
                                Some(prev) => prev.min(chunk.min_ts_ns),
                                None => chunk.min_ts_ns,
                            })
                        };
                        let next_newest = if chunk.row_count == 0 {
                            newest
                        } else {
                            Some(match newest {
                                Some(prev) => prev.max(chunk.max_ts_ns),
                                None => chunk.max_ts_ns,
                            })
                        };
                        (next_oldest, next_newest)
                    });
            let oldest_ts_ms = oldest_ns.map(|ns| ns / 1_000_000);
            let newest_ts_ms = newest_ns.map(|ns| ns / 1_000_000);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            // Issue #748 — downsample / continuous-aggregate / sweep
            // indicators. `downsample_policies` is the comma-joined
            // list of policy specs stored at CREATE TIMESERIES time;
            // `continuous_aggregate_*` reflect aggregates whose
            // declared `source` is this collection. `last_sweep_ms` is
            // pinned `NULL` (unavailable) because the retention
            // registry tracks a global `last_sweep_unix_ns` rather
            // than per-collection sweep state — see AC #3.
            let downsample_policies = read_downsample_policies(store.as_ref(), &collection.name);
            let downsample_policies_value = if downsample_policies.is_empty() {
                Value::Null
            } else {
                Value::text(downsample_policies.join(","))
            };
            let mut ca_names: Vec<String> = db
                .continuous_aggregates()
                .list()
                .into_iter()
                .filter(|spec| spec.source == collection.name)
                .map(|spec| spec.name)
                .collect();
            ca_names.sort();
            let ca_count = ca_names.len() as u64;
            let ca_names_value = if ca_names.is_empty() {
                Value::Null
            } else {
                Value::text(ca_names.join(","))
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::Boolean(is_hypertable),
                    time_column.map(Value::text).unwrap_or(Value::Null),
                    chunk_interval_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(chunk_count),
                    // Retention is stored in the runtime's default-TTL
                    // map, not on the catalog descriptor — read it
                    // back the same way the retention sweeper does.
                    db.collection_default_ttl_ms(&collection.name)
                        .or(collection.retention_duration_ms)
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    collection
                        .session_key
                        .clone()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    collection
                        .session_gap_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(collection.entities as u64),
                    oldest_ts_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    newest_ts_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                    downsample_policies_value,
                    Value::UnsignedInteger(ca_count),
                    ca_names_value,
                    Value::Null,
                ],
            )
        })
        .collect()
}
/// Issue #748 — read the `downsample_policies` array persisted in
/// the `red_timeseries_meta` collection by
/// `super::impl_timeseries::save_timeseries_metadata`. Returns the
/// sorted list of policy spec strings (empty when none / collection
/// is not a timeseries).
fn read_downsample_policies(store: &UnifiedStore, collection: &str) -> Vec<String> {
    const META: &str = "red_timeseries_meta";
    let Some(manager) = store.get_collection(META) else {
        return Vec::new();
    };
    let rows = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            row.get_field("series").is_some_and(
                |value| matches!(value, Value::Text(candidate) if &**candidate == collection),
            )
        })
    });
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        let Some(row_data) = row.data.as_row() else {
            continue;
        };
        let Some(Value::Array(specs)) = row_data.get_field("downsample_policies") else {
            continue;
        };
        for value in specs {
            if let Value::Text(s) = value {
                out.push(s.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
/// Issue #748 — per-chunk hypertable metadata. One row per
/// `(hypertable, chunk_start_ns)` covering every registered
/// hypertable visible under the active tenant / scope. Empty chunks
/// report `NULL` for `min_ts_ms` / `max_ts_ms` rather than leaking
/// the registry's `u64::MAX` sentinel.
pub(super) fn hypertable_chunks_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        HYPERTABLE_CHUNK_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let db = runtime.db();
    let store = db.store();
    let registry = db.hypertables();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let mut hypertables = registry.names();
    hypertables.sort();

    let mut rows: Vec<UnifiedRecord> = Vec::new();
    for name in hypertables {
        if !collection_is_visible(&name, visible_collections) {
            continue;
        }
        let owner_tenant = collection_tenant(store.as_ref(), &name);
        if let Some(scope) = tenant {
            if let Some(owner) = owner_tenant.as_deref() {
                if owner != scope {
                    continue;
                }
            }
        }
        let visible_tenant = owner_tenant.as_deref().or(tenant);
        let Some(spec) = registry.get(&name) else {
            continue;
        };
        let mut chunks = registry.show_chunks(&name);
        chunks.sort_by_key(|c| c.id.start_ns);
        for chunk in chunks {
            let has_rows = chunk.row_count > 0;
            let min_ts_ms = if has_rows {
                Value::UnsignedInteger(chunk.min_ts_ns / 1_000_000)
            } else {
                Value::Null
            };
            let max_ts_ms = if has_rows {
                Value::UnsignedInteger(chunk.max_ts_ns / 1_000_000)
            } else {
                Value::Null
            };
            let ttl_override_ms = chunk
                .ttl_override_ns
                .map(|ns| Value::UnsignedInteger(ns / 1_000_000))
                .unwrap_or(Value::Null);
            let effective_ttl_ns = chunk.effective_ttl_ns(spec.default_ttl_ns);
            let effective_ttl_ms = effective_ttl_ns
                .map(|ns| Value::UnsignedInteger(ns / 1_000_000))
                .unwrap_or(Value::Null);
            // `expiry_ns` is `max_ts_ns + effective_ttl_ns`; only
            // meaningful when the chunk has actually observed a row.
            let expiry_ms = match (has_rows, chunk.expiry_ns(spec.default_ttl_ns)) {
                (true, Some(ns)) => Value::UnsignedInteger(ns / 1_000_000),
                _ => Value::Null,
            };
            let is_expired = has_rows && chunk.is_expired_at(now_ns, spec.default_ttl_ns);
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name.clone()),
                    Value::UnsignedInteger(chunk.id.start_ns / 1_000_000),
                    Value::UnsignedInteger(chunk.end_ns_exclusive / 1_000_000),
                    Value::UnsignedInteger(chunk.row_count),
                    min_ts_ms,
                    max_ts_ms,
                    Value::Boolean(chunk.sealed),
                    ttl_override_ms,
                    effective_ttl_ms,
                    expiry_ms,
                    Value::Boolean(is_expired),
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                ],
            ));
        }
    }
    rows
}
/// Issue #748 — writes-by-cohort bucketing. For each hypertable
/// visible under the active scope, scans rows from the backing
/// segment manager, reads the time column, and accumulates a count
/// per `(bucket_size_ms, bucket_start_ms)` for each of the three
/// canonical cohort sizes (1m / 5m / 10m). Empty buckets are not
/// emitted. `writes_count` is held at `NULL` until reliable
/// WAL/operation telemetry exists — the thread-discussion decision
/// requires we distinguish event-time row counts from actual write
/// throughput, and not paper over the gap by labelling the former.
///
/// Filters: an optional `WHERE collection = 'x'` narrows to a single
/// hypertable; an optional `WHERE bucket_size_ms = N` narrows to a
/// single cohort size. Both are evaluated by inspecting the
/// `TableQuery` filter — heavier filters fall through (the row set
/// is filtered by the normal execution path after this snapshot
/// runs).
pub(super) fn timeseries_writes_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> Vec<UnifiedRecord> {
    const BUCKET_SIZES_MS: [u64; 3] = [60_000, 300_000, 600_000];

    let schema = Arc::new(
        TIMESERIES_WRITES_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let collection_filter = extract_text_eq(query, "collection");
    let bucket_filter = extract_uint_eq(query, "bucket_size_ms");

    let db = runtime.db();
    let store = db.store();
    let registry = db.hypertables();
    let mut hypertables = registry.names();
    hypertables.sort();

    let mut rows: Vec<UnifiedRecord> = Vec::new();
    for name in hypertables {
        if let Some(want) = collection_filter.as_deref() {
            if want != name {
                continue;
            }
        }
        if !collection_is_visible(&name, visible_collections) {
            continue;
        }
        let owner_tenant = collection_tenant(store.as_ref(), &name);
        if let Some(scope) = tenant {
            if let Some(owner) = owner_tenant.as_deref() {
                if owner != scope {
                    continue;
                }
            }
        }
        let Some(spec) = registry.get(&name) else {
            continue;
        };
        let Some(manager) = store.get_collection(&name) else {
            continue;
        };
        let time_col = spec.time_column.clone();
        let mut active_sizes: Vec<u64> = match bucket_filter {
            Some(size) if BUCKET_SIZES_MS.contains(&size) => vec![size],
            Some(_) => continue, // unsupported bucket size — skip silently
            None => BUCKET_SIZES_MS.to_vec(),
        };
        active_sizes.sort();

        // Scan the backing collection once, fold every row's time
        // column into every active cohort.
        let buckets: BTreeMap<(u64, u64), u64> = manager.fold_entities_parallel(
            BTreeMap::new,
            |mut local, entity| {
                let Some(row) = entity.data.as_row() else {
                    return local;
                };
                let Some(value) = row.get_field(&time_col) else {
                    return local;
                };
                let Some(ts_ns) = value_to_unsigned_ns(value) else {
                    return local;
                };
                let ts_ms = ts_ns / 1_000_000;
                for size in &active_sizes {
                    let bucket = (ts_ms / *size) * *size;
                    *local.entry((*size, bucket)).or_insert(0) += 1;
                }
                local
            },
            |mut a, b| {
                for (k, v) in b {
                    *a.entry(k).or_insert(0) += v;
                }
                a
            },
        );

        for ((bucket_size_ms, bucket_start_ms), events_count) in buckets {
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name.clone()),
                    Value::UnsignedInteger(bucket_size_ms),
                    Value::UnsignedInteger(bucket_start_ms),
                    Value::UnsignedInteger(events_count),
                    // `writes_count` — actual write throughput is
                    // unavailable until WAL/operation telemetry
                    // exists; AC #3 + thread-discussion require we
                    // surface NULL rather than inferring it from the
                    // event-time row count.
                    Value::Null,
                ],
            ));
        }
    }
    rows
}
/// Extract `WHERE <column> = '<text>'` from a TableQuery filter for
/// snapshot-time pushdown. Returns `None` if the filter is missing,
/// not an equality, or compares a different column.
fn extract_text_eq(query: &TableQuery, column: &str) -> Option<String> {
    match query.filter.as_ref()? {
        Filter::Compare {
            field: FieldRef::TableColumn { column: c, .. },
            op: CompareOp::Eq,
            value: Value::Text(text),
        } if c == column => Some(text.to_string()),
        _ => None,
    }
}
/// Extract `WHERE <column> = <unsigned int>` (accepting Int / BigInt
/// / Unsigned variants). Returns `None` if the filter is missing or
/// not an integer equality on the requested column.
fn extract_uint_eq(query: &TableQuery, column: &str) -> Option<u64> {
    match query.filter.as_ref()? {
        Filter::Compare {
            field: FieldRef::TableColumn { column: c, .. },
            op: CompareOp::Eq,
            value,
        } if c == column => match value {
            Value::UnsignedInteger(n) => Some(*n),
            Value::Integer(n) | Value::BigInt(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        },
        _ => None,
    }
}
/// Convert a `Value` representing a unix-nanosecond timestamp into a
/// `u64`. Mirrors the loose acceptance the hypertable INSERT path
/// already applies (`Value::Integer | BigInt | UnsignedInteger`).
fn value_to_unsigned_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(n) => Some(*n),
        Value::Integer(n) | Value::BigInt(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}
/// Issue #746 — typed `red.graphs` projection. Filtered to
/// `model = graph`. Per-collection node / edge counts are produced by
/// a single scan over the collection's segment manager (the catalog
/// snapshot's `entities` total lumps nodes and edges together for
/// graph collections, which the UI cannot split). `node_labels` /
/// `edge_labels` are deterministic sorted arrays so test assertions
/// and the toolbar both see a stable shape. `supports_viewport` is
/// the stable indicator the explorer keys on; the viewport contract
/// itself landed in #744.
pub(super) fn graphs_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        GRAPH_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Graph)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let (node_count, edge_count, node_labels, edge_labels) =
                graph_counts(store.as_ref(), &collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(node_count),
                    Value::UnsignedInteger(edge_count),
                    Value::Array(node_labels.into_iter().map(Value::text).collect()),
                    Value::Array(edge_labels.into_iter().map(Value::text).collect()),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
/// Issue #746 — single-pass scan over a graph collection's segment
/// manager that returns `(node_count, edge_count, node_labels,
/// edge_labels)`. Labels are deduplicated and returned in sorted
/// order so the typed projection has a stable shape across calls.
fn graph_counts(store: &UnifiedStore, collection: &str) -> (u64, u64, Vec<String>, Vec<String>) {
    let Some(manager) = store.get_collection(collection) else {
        return (0, 0, Vec::new(), Vec::new());
    };
    let mut node_count: u64 = 0;
    let mut edge_count: u64 = 0;
    let mut node_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut edge_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entity in manager.query_all(|_| true) {
        match &entity.kind {
            crate::storage::EntityKind::GraphNode(node) => {
                node_count = node_count.saturating_add(1);
                if !node.node_type.is_empty() {
                    node_labels.insert(node.node_type.clone());
                }
            }
            crate::storage::EntityKind::GraphEdge(edge) => {
                edge_count = edge_count.saturating_add(1);
                if !edge.label.is_empty() {
                    edge_labels.insert(edge.label.clone());
                }
            }
            _ => {}
        }
    }
    (
        node_count,
        edge_count,
        node_labels.into_iter().collect(),
        edge_labels.into_iter().collect(),
    )
}
/// Issue #747 — typed `red.metrics` projection. One row per metric
/// descriptor registered through `CREATE METRIC`. `labels` / `unit` /
/// `retention_ms` columns exist for schema stability but are populated
/// as `NULL` until the descriptor catalog tracks them. Descriptors
/// are not tenant-owned today, so the visibility behavior matches
/// `red.analytics.metrics`: cluster admins and tenant sessions both
/// see the full catalog. `_tenant` and `_visible_collections` arguments
/// are accepted for shape parity with the other typed-relation
/// snapshots and to leave room for future tenant scoping without
/// breaking callers.
pub(super) fn metrics_snapshot(
    runtime: &RedDBRuntime,
    _tenant: Option<&str>,
    _visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        METRICS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::metric_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.kind),
                    Value::text(entry.role),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Boolean(true),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}
/// Issue #745 — count the rows that look like documents (have a JSON
/// or text `body` field) and the distinct top-level field names seen
/// across them. Mirrors `infer_document_columns` so the two surfaces
/// cannot drift.
fn document_counts(runtime: &RedDBRuntime, collection: &str) -> (u64, u64) {
    let mut document_count: u64 = 0;
    let mut field_names: HashSet<String> = HashSet::new();
    for (_, entity) in runtime
        .db()
        .store()
        .query_all(|entity| entity.kind.collection() == collection)
    {
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if !row
            .iter_fields()
            .any(|(name, value)| name == "body" && matches!(value, Value::Json(_) | Value::Text(_)))
        {
            continue;
        }
        document_count = document_count.saturating_add(1);
        for (name, _) in row.iter_fields() {
            field_names.insert(name.to_string());
        }
        // Post-cutover (PRD-1398) the canonical document lives only in the
        // binary `body` container; offset-read its top-level fields so the
        // count matches the columns surfaced by `infer_document_columns`.
        if let Some(Value::Json(bytes)) = row.get_field("body") {
            if let Some(body_fields) = crate::document_body::body_fields(bytes) {
                for (name, _) in body_fields {
                    field_names.insert(name);
                }
            }
        }
    }
    (document_count, field_names.len() as u64)
}
