use super::*;

pub(super) fn manifest_to_json(manifest: &SchemaManifest) -> JsonValue {
    let mut options = Map::new();
    options.insert(
        "mode".to_string(),
        JsonValue::String(match manifest.options.mode {
            StorageMode::Persistent => "persistent".to_string(),
        }),
    );
    options.insert(
        "data_path".to_string(),
        match &manifest.options.data_path {
            Some(path) => JsonValue::String(path.display().to_string()),
            None => JsonValue::Null,
        },
    );
    options.insert(
        "read_only".to_string(),
        JsonValue::Bool(manifest.options.read_only),
    );
    options.insert(
        "create_if_missing".to_string(),
        JsonValue::Bool(manifest.options.create_if_missing),
    );
    options.insert(
        "verify_checksums".to_string(),
        JsonValue::Bool(manifest.options.verify_checksums),
    );
    options.insert(
        "durability_mode".to_string(),
        JsonValue::String(manifest.options.durability_mode.as_str().to_string()),
    );
    options.insert(
        "group_commit_window_ms".to_string(),
        JsonValue::Number(manifest.options.group_commit.window_ms as f64),
    );
    options.insert(
        "group_commit_max_statements".to_string(),
        JsonValue::Number(manifest.options.group_commit.max_statements as f64),
    );
    options.insert(
        "group_commit_max_wal_bytes".to_string(),
        JsonValue::Number(manifest.options.group_commit.max_wal_bytes as f64),
    );
    options.insert(
        "auto_checkpoint_pages".to_string(),
        JsonValue::Number(manifest.options.auto_checkpoint_pages as f64),
    );
    options.insert(
        "cache_pages".to_string(),
        JsonValue::Number(manifest.options.cache_pages as f64),
    );
    options.insert(
        "snapshot_retention".to_string(),
        JsonValue::Number(manifest.options.snapshot_retention as f64),
    );
    options.insert(
        "export_retention".to_string(),
        JsonValue::Number(manifest.options.export_retention as f64),
    );
    options.insert(
        "force_create".to_string(),
        JsonValue::Bool(manifest.options.force_create),
    );
    options.insert(
        "capabilities".to_string(),
        JsonValue::Array(
            manifest
                .options
                .feature_gates
                .as_slice()
                .into_iter()
                .map(|capability| JsonValue::String(capability.as_str().to_string()))
                .collect(),
        ),
    );
    options.insert(
        "metadata".to_string(),
        JsonValue::Object(
            manifest
                .options
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );

    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(manifest.format_version as f64),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(manifest.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(manifest.updated_at_unix_ms),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(manifest.collection_count as f64),
    );
    object.insert("options".to_string(), JsonValue::Object(options));
    JsonValue::Object(object)
}

pub(super) fn manifest_from_json(value: &JsonValue) -> io::Result<SchemaManifest> {
    let object = expect_object(value, "manifest")?;
    let options_object = expect_object(json_required(object, "options")?, "manifest.options")?;
    let mut options = RedDBOptions {
        mode: match json_string_required(options_object, "mode")?.as_str() {
            "persistent" | "in_memory" => StorageMode::Persistent,
            other => {
                return Err(invalid_data(format!(
                    "unsupported storage mode in manifest: {other}"
                )))
            }
        },
        data_path: options_object
            .get("data_path")
            .and_then(JsonValue::as_str)
            .map(PathBuf::from),
        read_only: json_bool_required(options_object, "read_only")?,
        create_if_missing: json_bool_required(options_object, "create_if_missing")?,
        verify_checksums: json_bool_required(options_object, "verify_checksums")?,
        durability_mode: options_object
            .get("durability_mode")
            .and_then(JsonValue::as_str)
            .and_then(crate::api::DurabilityMode::from_str)
            .unwrap_or(crate::api::DurabilityMode::Strict),
        group_commit: crate::api::GroupCommitOptions {
            window_ms: options_object
                .get("group_commit_window_ms")
                .map(|_value| json_u64_required(options_object, "group_commit_window_ms"))
                .transpose()?
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_WINDOW_MS)
                .max(1),
            max_statements: options_object
                .get("group_commit_max_statements")
                .map(|_value| json_usize_required(options_object, "group_commit_max_statements"))
                .transpose()?
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_MAX_STATEMENTS)
                .max(1),
            max_wal_bytes: options_object
                .get("group_commit_max_wal_bytes")
                .map(|_value| json_u64_required(options_object, "group_commit_max_wal_bytes"))
                .transpose()?
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_MAX_WAL_BYTES)
                .max(1),
        },
        auto_checkpoint_pages: json_u32_required(options_object, "auto_checkpoint_pages")?,
        cache_pages: json_usize_required(options_object, "cache_pages")?,
        snapshot_retention: options_object
            .get("snapshot_retention")
            .map(|_value| json_usize_required(options_object, "snapshot_retention"))
            .transpose()?
            .unwrap_or(crate::api::DEFAULT_SNAPSHOT_RETENTION)
            .max(1),
        export_retention: options_object
            .get("export_retention")
            .map(|_value| json_usize_required(options_object, "export_retention"))
            .transpose()?
            .unwrap_or(crate::api::DEFAULT_EXPORT_RETENTION)
            .max(1),
        force_create: json_bool_required(options_object, "force_create")?,
        metadata: options_object
            .get("metadata")
            .and_then(JsonValue::as_object)
            .map(|metadata| {
                metadata
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(capabilities) = options_object
        .get("capabilities")
        .and_then(JsonValue::as_array)
    {
        options.feature_gates =
            capabilities
                .iter()
                .fold(Default::default(), |set, value| match value.as_str() {
                    Some("table") => set.with(crate::api::Capability::Table),
                    Some("graph") => set.with(crate::api::Capability::Graph),
                    Some("vector") => set.with(crate::api::Capability::Vector),
                    Some("fulltext") => set.with(crate::api::Capability::FullText),
                    Some("security") => set.with(crate::api::Capability::Security),
                    Some("encryption") => set.with(crate::api::Capability::Encryption),
                    _ => set,
                });
    }

    Ok(SchemaManifest {
        format_version: json_u32_required(object, "format_version")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        options,
        collection_count: json_usize_required(object, "collection_count")?,
    })
}

pub(super) fn catalog_to_json(catalog: &CatalogSnapshot) -> JsonValue {
    let mut stats = Map::new();
    for (name, stat) in &catalog.stats_by_collection {
        let mut entry = Map::new();
        entry.insert(
            "entities".to_string(),
            JsonValue::Number(stat.entities as f64),
        );
        entry.insert(
            "cross_refs".to_string(),
            JsonValue::Number(stat.cross_refs as f64),
        );
        entry.insert(
            "segments".to_string(),
            JsonValue::Number(stat.segments as f64),
        );
        stats.insert(name.clone(), JsonValue::Object(entry));
    }

    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(catalog.name.clone()));
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(catalog.total_entities as f64),
    );
    object.insert(
        "total_collections".to_string(),
        JsonValue::Number(catalog.total_collections as f64),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(
            catalog
                .updated_at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        ),
    );
    object.insert("stats_by_collection".to_string(), JsonValue::Object(stats));
    JsonValue::Object(object)
}

pub(super) fn catalog_from_json(value: &JsonValue) -> io::Result<CatalogSnapshot> {
    let object = expect_object(value, "catalog")?;
    let stats = expect_object(
        json_required(object, "stats_by_collection")?,
        "catalog.stats",
    )?;
    let mut stats_by_collection = BTreeMap::new();
    for (name, value) in stats {
        let entry = expect_object(value, "catalog.stats entry")?;
        stats_by_collection.insert(
            name.clone(),
            CollectionStats {
                entities: json_usize_required(entry, "entities")?,
                cross_refs: json_usize_required(entry, "cross_refs")?,
                segments: json_usize_required(entry, "segments")?,
            },
        );
    }

    Ok(CatalogSnapshot {
        name: json_string_required(object, "name")?,
        total_entities: json_usize_required(object, "total_entities")?,
        total_collections: json_usize_required(object, "total_collections")?,
        stats_by_collection,
        updated_at: UNIX_EPOCH
            + std::time::Duration::from_millis(
                json_u128_required(object, "updated_at_unix_ms")?
                    .try_into()
                    .unwrap_or(u64::MAX),
            ),
    })
}

pub(super) fn collection_contract_to_json(contract: &CollectionContract) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(contract.name.clone()));
    object.insert(
        "declared_model".to_string(),
        JsonValue::String(collection_model_as_str(contract.declared_model).to_string()),
    );
    object.insert(
        "schema_mode".to_string(),
        JsonValue::String(schema_mode_as_str(contract.schema_mode).to_string()),
    );
    object.insert(
        "origin".to_string(),
        JsonValue::String(contract.origin.as_str().to_string()),
    );
    object.insert(
        "version".to_string(),
        JsonValue::Number(contract.version as f64),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(contract.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(contract.updated_at_unix_ms),
    );
    object.insert(
        "default_ttl_ms".to_string(),
        contract
            .default_ttl_ms
            .map(json_u64)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "vector_dimension".to_string(),
        contract
            .vector_dimension
            .map(|dimension| JsonValue::Number(dimension as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "vector_metric".to_string(),
        contract
            .vector_metric
            .map(|metric| JsonValue::String(distance_metric_as_str(metric).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "context_index_fields".to_string(),
        JsonValue::Array(
            contract
                .context_index_fields
                .iter()
                .map(|field| JsonValue::String(field.clone()))
                .collect(),
        ),
    );
    object.insert(
        "declared_columns".to_string(),
        JsonValue::Array(
            contract
                .declared_columns
                .iter()
                .map(declared_column_contract_to_json)
                .collect(),
        ),
    );
    object.insert(
        "timestamps_enabled".to_string(),
        JsonValue::Bool(contract.timestamps_enabled),
    );
    object.insert(
        "context_index_enabled".to_string(),
        JsonValue::Bool(contract.context_index_enabled),
    );
    object.insert(
        "metrics_raw_retention_ms".to_string(),
        contract
            .metrics_raw_retention_ms
            .map(json_u64)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "metrics_rollup_policies".to_string(),
        JsonValue::Array(
            contract
                .metrics_rollup_policies
                .iter()
                .map(|policy| JsonValue::String(policy.clone()))
                .collect(),
        ),
    );
    object.insert(
        "metrics_tenant_identity".to_string(),
        contract
            .metrics_tenant_identity
            .as_ref()
            .map(|identity| JsonValue::String(identity.clone()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "metrics_namespace".to_string(),
        contract
            .metrics_namespace
            .as_ref()
            .map(|namespace| JsonValue::String(namespace.clone()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "append_only".to_string(),
        JsonValue::Bool(contract.append_only),
    );
    object.insert(
        "subscriptions".to_string(),
        JsonValue::Array(
            contract
                .subscriptions
                .iter()
                .map(subscription_descriptor_to_json)
                .collect(),
        ),
    );
    object.insert(
        "analytics_config".to_string(),
        JsonValue::Array(
            contract
                .analytics_config
                .iter()
                .map(analytics_view_descriptor_to_json)
                .collect(),
        ),
    );
    object.insert(
        "session_key".to_string(),
        contract
            .session_key
            .as_ref()
            .map(|name| JsonValue::String(name.clone()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "session_gap_ms".to_string(),
        contract
            .session_gap_ms
            .map(|ms| JsonValue::Number(ms as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "retention_duration_ms".to_string(),
        contract
            .retention_duration_ms
            .map(json_u64)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "analytical_storage".to_string(),
        contract
            .analytical_storage
            .as_ref()
            .map(analytical_storage_to_json)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "table_def".to_string(),
        contract
            .table_def
            .as_ref()
            .map(|table_def| JsonValue::String(hex::encode(table_def.to_bytes())))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn analytical_storage_to_json(cfg: &crate::catalog::AnalyticalStorageConfig) -> JsonValue {
    let mut object = Map::new();
    object.insert("columnar".to_string(), JsonValue::Bool(cfg.columnar));
    object.insert(
        "time_key".to_string(),
        JsonValue::String(cfg.time_key.clone()),
    );
    object.insert(
        "order_by_key".to_string(),
        cfg.order_by_key
            .as_ref()
            .map(|k| JsonValue::String(k.clone()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn analytical_storage_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticalStorageConfig> {
    let object = expect_object(value, "analytical_storage")?;
    Ok(crate::catalog::AnalyticalStorageConfig {
        columnar: object
            .get("columnar")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        time_key: json_string_required(object, "time_key")?,
        order_by_key: object
            .get("order_by_key")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
    })
}

pub(super) fn collection_contract_from_json(value: &JsonValue) -> io::Result<CollectionContract> {
    let object = expect_object(value, "collection_contract")?;
    let table_def = match object.get("table_def") {
        Some(JsonValue::String(encoded)) => {
            let bytes = hex::decode(encoded).map_err(|err| {
                invalid_data(format!("invalid collection contract table_def hex: {err}"))
            })?;
            Some(
                crate::storage::schema::TableDef::from_bytes(&bytes).map_err(|err| {
                    invalid_data(format!(
                        "invalid collection contract table_def payload: {err}"
                    ))
                })?,
            )
        }
        Some(JsonValue::Null) | None => None,
        Some(_) => {
            return Err(invalid_data(
                "collection_contract.table_def must be a hex string or null".to_string(),
            ))
        }
    };

    Ok(CollectionContract {
        name: json_string_required(object, "name")?,
        declared_model: collection_model_from_str(&json_string_required(
            object,
            "declared_model",
        )?)?,
        schema_mode: schema_mode_from_str(&json_string_required(object, "schema_mode")?)?,
        origin: contract_origin_from_str(&json_string_required(object, "origin")?)?,
        version: json_u32_required(object, "version")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        default_ttl_ms: match object.get("default_ttl_ms") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        vector_dimension: match object.get("vector_dimension") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(json_usize_value(value)?),
        },
        vector_metric: match object.get("vector_metric") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(distance_metric_from_str(value.as_str().ok_or_else(
                || invalid_data("collection_contract.vector_metric must be a string".to_string()),
            )?)?),
        },
        context_index_fields: object
            .get("context_index_fields")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(|value| value.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        declared_columns: object
            .get("declared_columns")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(declared_column_contract_from_json)
                    .collect::<io::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
        table_def,
        timestamps_enabled: object
            .get("timestamps_enabled")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        // Legacy sidecars written before per-table opt-in lack this key.
        // Pre-PR behavior was "context index on unless REDDB_DISABLE_CONTEXT_INDEX";
        // defaulting missing → true preserves that for existing tables on upgrade.
        context_index_enabled: object
            .get("context_index_enabled")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true),
        metrics_raw_retention_ms: match object.get("metrics_raw_retention_ms") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        metrics_rollup_policies: object
            .get("metrics_rollup_policies")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        metrics_tenant_identity: object
            .get("metrics_tenant_identity")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        metrics_namespace: object
            .get("metrics_namespace")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        // Legacy sidecars lack the append_only flag — default false
        // (pre-feature behaviour: tables were always mutable).
        append_only: object
            .get("append_only")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        subscriptions: object
            .get("subscriptions")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(subscription_descriptor_from_json)
                    .collect::<io::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
        // Legacy sidecars written before the analytics opt-in lack this
        // key — default to an empty config (no analytics views). Issue #800.
        analytics_config: object
            .get("analytics_config")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(analytics_view_descriptor_from_json)
                    .collect::<io::Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
        // Legacy sidecars lack the session_key / session_gap_ms keys —
        // default None preserves pre-feature behaviour (no SESSIONIZE
        // defaults). Issue #576 slice 1.
        session_key: object
            .get("session_key")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        session_gap_ms: match object.get("session_gap_ms") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        // Legacy sidecars lack the retention_duration_ms key — default
        // None preserves pre-feature behaviour (no retention filter).
        // Issue #580 slice 1.
        retention_duration_ms: match object.get("retention_duration_ms") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        // Legacy sidecars written before the columnar analytical-storage
        // seam lack this key — default None (row engine). PRD #850 Phase 1.
        analytical_storage: match object.get("analytical_storage") {
            Some(JsonValue::Null) | None => None,
            Some(value) => Some(analytical_storage_from_json(value)?),
        },
    })
}

fn subscription_descriptor_to_json(
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(subscription.name.clone()),
    );
    object.insert(
        "source".to_string(),
        JsonValue::String(subscription.source.clone()),
    );
    object.insert(
        "target_queue".to_string(),
        JsonValue::String(subscription.target_queue.clone()),
    );
    object.insert(
        "ops_filter".to_string(),
        JsonValue::Array(
            subscription
                .ops_filter
                .iter()
                .map(|op| JsonValue::String(op.as_str().to_string()))
                .collect(),
        ),
    );
    object.insert(
        "where_filter".to_string(),
        subscription
            .where_filter
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "redact_fields".to_string(),
        JsonValue::Array(
            subscription
                .redact_fields
                .iter()
                .map(|field| JsonValue::String(field.clone()))
                .collect(),
        ),
    );
    object.insert("enabled".to_string(), JsonValue::Bool(subscription.enabled));
    object.insert(
        "all_tenants".to_string(),
        JsonValue::Bool(subscription.all_tenants),
    );
    JsonValue::Object(object)
}

fn subscription_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::SubscriptionDescriptor> {
    let object = expect_object(value, "subscription_descriptor")?;
    let ops_filter = object
        .get("ops_filter")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    let op = value.as_str().ok_or_else(|| {
                        invalid_data("subscription_descriptor.ops_filter must contain strings")
                    })?;
                    crate::catalog::SubscriptionOperation::from_str(op).ok_or_else(|| {
                        invalid_data(format!(
                            "unsupported subscription operation in catalog: {op}"
                        ))
                    })
                })
                .collect::<io::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let redact_fields = object
        .get("redact_fields")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(|value| value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(crate::catalog::SubscriptionDescriptor {
        name: object
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        source: json_string_required(object, "source")?,
        target_queue: json_string_required(object, "target_queue")?,
        ops_filter,
        where_filter: match object.get("where_filter") {
            Some(JsonValue::String(value)) => Some(value.clone()),
            Some(JsonValue::Null) | None => None,
            Some(_) => {
                return Err(invalid_data(
                    "subscription_descriptor.where_filter must be a string or null".to_string(),
                ))
            }
        },
        redact_fields,
        enabled: object
            .get("enabled")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true),
        all_tenants: object
            .get("all_tenants")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
    })
}

fn analytics_view_descriptor_to_json(view: &crate::catalog::AnalyticsViewDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "output".to_string(),
        JsonValue::String(view.output.as_str().to_string()),
    );
    object.insert(
        "algorithm".to_string(),
        view.algorithm
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "resolution".to_string(),
        view.resolution
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "max_iterations".to_string(),
        view.max_iterations
            .map(|n| JsonValue::Number(n as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "tolerance".to_string(),
        view.tolerance
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn analytics_view_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticsViewDescriptor> {
    let object = expect_object(value, "analytics_view_descriptor")?;
    let output_str = json_string_required(object, "output")?;
    let output = crate::catalog::AnalyticsOutput::from_str(&output_str).ok_or_else(|| {
        invalid_data(format!(
            "analytics_view_descriptor.output has unsupported value: {output_str}"
        ))
    })?;
    Ok(crate::catalog::AnalyticsViewDescriptor {
        output,
        algorithm: object
            .get("algorithm")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        resolution: object.get("resolution").and_then(JsonValue::as_f64),
        max_iterations: object.get("max_iterations").and_then(JsonValue::as_i64),
        tolerance: object.get("tolerance").and_then(JsonValue::as_f64),
    })
}

fn declared_column_contract_to_json(column: &DeclaredColumnContract) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(column.name.clone()));
    object.insert(
        "data_type".to_string(),
        JsonValue::String(column.data_type.clone()),
    );
    object.insert(
        "sql_type".to_string(),
        column
            .sql_type
            .as_ref()
            .map(sql_type_name_to_json)
            .unwrap_or(JsonValue::Null),
    );
    object.insert("not_null".to_string(), JsonValue::Bool(column.not_null));
    object.insert(
        "default".to_string(),
        column
            .default
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "compress".to_string(),
        column
            .compress
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert("unique".to_string(), JsonValue::Bool(column.unique));
    object.insert(
        "primary_key".to_string(),
        JsonValue::Bool(column.primary_key),
    );
    object.insert(
        "enum_variants".to_string(),
        JsonValue::Array(
            column
                .enum_variants
                .iter()
                .map(|variant| JsonValue::String(variant.clone()))
                .collect(),
        ),
    );
    object.insert(
        "array_element".to_string(),
        column
            .array_element
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "decimal_precision".to_string(),
        column
            .decimal_precision
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn declared_column_contract_from_json(value: &JsonValue) -> io::Result<DeclaredColumnContract> {
    let object = expect_object(value, "declared_column_contract")?;
    Ok(DeclaredColumnContract {
        name: json_string_required(object, "name")?,
        data_type: json_string_required(object, "data_type")?,
        sql_type: object
            .get("sql_type")
            .map(sql_type_name_from_json)
            .transpose()?
            .flatten()
            .or_else(|| {
                object
                    .get("data_type")
                    .and_then(JsonValue::as_str)
                    .map(crate::storage::schema::SqlTypeName::parse_declared)
            }),
        not_null: json_bool_required(object, "not_null")?,
        default: object
            .get("default")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        compress: match object.get("compress") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(json_u8_field_value(value)?),
        },
        unique: json_bool_required(object, "unique")?,
        primary_key: json_bool_required(object, "primary_key")?,
        enum_variants: object
            .get("enum_variants")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(|value| value.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        array_element: object
            .get("array_element")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        decimal_precision: match object.get("decimal_precision") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(json_u8_field_value(value)?),
        },
    })
}

fn sql_type_name_to_json(sql_type: &crate::storage::schema::SqlTypeName) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(sql_type.name.clone()));
    object.insert(
        "modifiers".to_string(),
        JsonValue::Array(
            sql_type
                .modifiers
                .iter()
                .map(type_modifier_to_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn sql_type_name_from_json(
    value: &JsonValue,
) -> io::Result<Option<crate::storage::schema::SqlTypeName>> {
    match value {
        JsonValue::Null => Ok(None),
        JsonValue::Object(object) => {
            let name = json_string_required(object, "name")?;
            let modifiers = object
                .get("modifiers")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(type_modifier_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            Ok(Some(crate::storage::schema::SqlTypeName {
                name,
                modifiers,
            }))
        }
        _ => Err(invalid_data(
            "sql_type must be an object or null".to_string(),
        )),
    }
}

fn type_modifier_to_json(modifier: &crate::storage::schema::TypeModifier) -> JsonValue {
    let mut object = Map::new();
    match modifier {
        crate::storage::schema::TypeModifier::Number(value) => {
            object.insert("kind".to_string(), JsonValue::String("number".to_string()));
            object.insert("value".to_string(), JsonValue::Number(*value as f64));
        }
        crate::storage::schema::TypeModifier::Ident(value) => {
            object.insert("kind".to_string(), JsonValue::String("ident".to_string()));
            object.insert("value".to_string(), JsonValue::String(value.clone()));
        }
        crate::storage::schema::TypeModifier::StringLiteral(value) => {
            object.insert("kind".to_string(), JsonValue::String("string".to_string()));
            object.insert("value".to_string(), JsonValue::String(value.clone()));
        }
        crate::storage::schema::TypeModifier::Type(value) => {
            object.insert("kind".to_string(), JsonValue::String("type".to_string()));
            object.insert("value".to_string(), sql_type_name_to_json(value));
        }
    }
    JsonValue::Object(object)
}

fn type_modifier_from_json(value: &JsonValue) -> io::Result<crate::storage::schema::TypeModifier> {
    let object = expect_object(value, "type_modifier")?;
    let kind = json_string_required(object, "kind")?;
    match kind.as_str() {
        "number" => Ok(crate::storage::schema::TypeModifier::Number(
            json_u32_required(object, "value")?,
        )),
        "ident" => Ok(crate::storage::schema::TypeModifier::Ident(
            json_string_required(object, "value")?,
        )),
        "string" => Ok(crate::storage::schema::TypeModifier::StringLiteral(
            json_string_required(object, "value")?,
        )),
        "type" => {
            let value = object
                .get("value")
                .ok_or_else(|| invalid_data("missing type modifier value".to_string()))?;
            let nested = sql_type_name_from_json(value)?
                .ok_or_else(|| invalid_data("type modifier cannot be null".to_string()))?;
            Ok(crate::storage::schema::TypeModifier::Type(Box::new(nested)))
        }
        other => Err(invalid_data(format!(
            "unsupported type modifier kind: {other}"
        ))),
    }
}

fn json_u8_field_value(value: &JsonValue) -> io::Result<u8> {
    let number = value
        .as_f64()
        .ok_or_else(|| invalid_data("expected numeric u8 field value".to_string()))?;
    if !(0.0..=(u8::MAX as f64)).contains(&number) {
        return Err(invalid_data(format!(
            "u8 field value out of range: {number}"
        )));
    }
    Ok(number as u8)
}

fn collection_model_as_str(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Table => "table",
        crate::catalog::CollectionModel::Document => "document",
        crate::catalog::CollectionModel::Graph => "graph",
        crate::catalog::CollectionModel::Vector => "vector",
        crate::catalog::CollectionModel::Hll => "hll",
        crate::catalog::CollectionModel::Sketch => "sketch",
        crate::catalog::CollectionModel::Filter => "filter",
        crate::catalog::CollectionModel::Kv => "kv",
        crate::catalog::CollectionModel::Config => "config",
        crate::catalog::CollectionModel::Vault => "vault",
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
        crate::catalog::CollectionModel::Metrics => "metrics",
    }
}

fn collection_model_from_str(value: &str) -> io::Result<crate::catalog::CollectionModel> {
    match value {
        "table" => Ok(crate::catalog::CollectionModel::Table),
        "document" => Ok(crate::catalog::CollectionModel::Document),
        "graph" => Ok(crate::catalog::CollectionModel::Graph),
        "vector" => Ok(crate::catalog::CollectionModel::Vector),
        "hll" => Ok(crate::catalog::CollectionModel::Hll),
        "sketch" => Ok(crate::catalog::CollectionModel::Sketch),
        "filter" => Ok(crate::catalog::CollectionModel::Filter),
        "kv" => Ok(crate::catalog::CollectionModel::Kv),
        "config" => Ok(crate::catalog::CollectionModel::Config),
        "vault" => Ok(crate::catalog::CollectionModel::Vault),
        "mixed" => Ok(crate::catalog::CollectionModel::Mixed),
        "timeseries" => Ok(crate::catalog::CollectionModel::TimeSeries),
        "queue" => Ok(crate::catalog::CollectionModel::Queue),
        "metrics" => Ok(crate::catalog::CollectionModel::Metrics),
        other => Err(invalid_data(format!(
            "unsupported collection contract model: {other}"
        ))),
    }
}

fn distance_metric_as_str(
    metric: crate::storage::engine::distance::DistanceMetric,
) -> &'static str {
    match metric {
        crate::storage::engine::distance::DistanceMetric::L2 => "l2",
        crate::storage::engine::distance::DistanceMetric::Cosine => "cosine",
        crate::storage::engine::distance::DistanceMetric::InnerProduct => "inner_product",
    }
}

fn distance_metric_from_str(
    value: &str,
) -> io::Result<crate::storage::engine::distance::DistanceMetric> {
    match value {
        "l2" | "L2" => Ok(crate::storage::engine::distance::DistanceMetric::L2),
        "cosine" | "COSINE" => Ok(crate::storage::engine::distance::DistanceMetric::Cosine),
        "inner_product" | "INNER_PRODUCT" => {
            Ok(crate::storage::engine::distance::DistanceMetric::InnerProduct)
        }
        other => Err(invalid_data(format!(
            "unsupported collection contract vector_metric: {other}"
        ))),
    }
}

fn schema_mode_as_str(mode: crate::catalog::SchemaMode) -> &'static str {
    match mode {
        crate::catalog::SchemaMode::Strict => "strict",
        crate::catalog::SchemaMode::SemiStructured => "semi_structured",
        crate::catalog::SchemaMode::Dynamic => "dynamic",
    }
}

fn schema_mode_from_str(value: &str) -> io::Result<crate::catalog::SchemaMode> {
    match value {
        "strict" => Ok(crate::catalog::SchemaMode::Strict),
        "semi_structured" => Ok(crate::catalog::SchemaMode::SemiStructured),
        "dynamic" => Ok(crate::catalog::SchemaMode::Dynamic),
        other => Err(invalid_data(format!(
            "unsupported collection contract schema mode: {other}"
        ))),
    }
}

fn contract_origin_from_str(value: &str) -> io::Result<ContractOrigin> {
    match value {
        "explicit" => Ok(ContractOrigin::Explicit),
        "implicit" => Ok(ContractOrigin::Implicit),
        "migrated" => Ok(ContractOrigin::Migrated),
        other => Err(invalid_data(format!(
            "unsupported collection contract origin: {other}"
        ))),
    }
}

pub(super) fn superblock_to_json(superblock: &SuperblockHeader) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_superblock_json(superblock)
            .expect("reddb-file must encode physical superblock JSON"),
    )
}

pub(super) fn superblock_from_json(value: &JsonValue) -> io::Result<SuperblockHeader> {
    reddb_file::decode_physical_superblock_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical superblock: {err}")))
}

pub(super) fn manifest_event_to_json(event: &ManifestEvent) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_manifest_event_json(event)
            .expect("reddb-file must encode physical manifest event JSON"),
    )
}

pub(super) fn manifest_event_from_json(value: &JsonValue) -> io::Result<ManifestEvent> {
    reddb_file::decode_physical_manifest_event_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical manifest event: {err}")))
}

pub(super) fn manifest_pointers_to_json(pointers: &ManifestPointers) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_manifest_pointers_json(pointers)
            .expect("reddb-file must encode physical manifest pointers JSON"),
    )
}

pub(super) fn manifest_pointers_from_json(value: &JsonValue) -> io::Result<ManifestPointers> {
    reddb_file::decode_physical_manifest_pointers_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical manifest pointers: {err}")))
}

pub(super) fn block_reference_to_json(reference: BlockReference) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_block_reference_json(reference)
            .expect("reddb-file must encode physical block reference JSON"),
    )
}

pub(super) fn block_reference_from_json(value: &JsonValue) -> io::Result<BlockReference> {
    reddb_file::decode_physical_block_reference_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical block reference: {err}")))
}

fn file_json_to_server_json(json: String) -> JsonValue {
    crate::serde_json::from_str(&json).expect("reddb-file emitted JSON the server can parse")
}

pub(super) fn snapshot_descriptor_to_json(snapshot: &SnapshotDescriptor) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_snapshot_descriptor_json(snapshot)
            .expect("reddb-file must encode physical snapshot descriptor JSON"),
    )
}

pub(super) fn snapshot_descriptor_from_json(value: &JsonValue) -> io::Result<SnapshotDescriptor> {
    reddb_file::decode_physical_snapshot_descriptor_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical snapshot descriptor: {err}")))
}

pub(super) fn index_state_to_json(index: &PhysicalIndexState) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_persisted_physical_index_state_json(&index_state_to_persisted(index))
            .expect("reddb-file must encode physical index state JSON"),
    )
}

pub(super) fn index_state_from_json(value: &JsonValue) -> io::Result<PhysicalIndexState> {
    let persisted =
        reddb_file::decode_persisted_physical_index_state_json(&value.to_string_compact())
            .map_err(|err| invalid_data(format!("decode physical index state: {err}")))?;
    Ok(PhysicalIndexState {
        name: persisted.name,
        kind: index_kind_from_str(&persisted.kind)?,
        collection: persisted.collection,
        enabled: persisted.enabled,
        entries: persisted.entries,
        estimated_memory_bytes: persisted.estimated_memory_bytes,
        last_refresh_ms: persisted.last_refresh_ms,
        backend: persisted.backend,
        artifact_kind: persisted.artifact_kind,
        artifact_root_page: persisted.artifact_root_page,
        artifact_checksum: persisted.artifact_checksum,
        build_state: persisted.build_state,
    })
}

fn index_state_to_persisted(index: &PhysicalIndexState) -> reddb_file::PersistedPhysicalIndexState {
    reddb_file::PersistedPhysicalIndexState {
        name: index.name.clone(),
        kind: index.kind.as_str().to_string(),
        collection: index.collection.clone(),
        enabled: index.enabled,
        entries: index.entries,
        estimated_memory_bytes: index.estimated_memory_bytes,
        last_refresh_ms: index.last_refresh_ms,
        backend: index.backend.clone(),
        artifact_kind: index.artifact_kind.clone(),
        artifact_root_page: index.artifact_root_page,
        artifact_checksum: index.artifact_checksum,
        build_state: index.build_state.clone(),
    }
}

pub(super) fn graph_projection_to_json(projection: &PhysicalGraphProjection) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_graph_projection_json(projection)
            .expect("reddb-file must encode physical graph projection JSON"),
    )
}

pub(super) fn graph_projection_from_json(value: &JsonValue) -> io::Result<PhysicalGraphProjection> {
    reddb_file::decode_physical_graph_projection_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical graph projection: {err}")))
}

pub(super) fn analytics_job_to_json(job: &PhysicalAnalyticsJob) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_analytics_job_json(job)
            .expect("reddb-file must encode physical analytics job JSON"),
    )
}

pub(super) fn analytics_job_from_json(value: &JsonValue) -> io::Result<PhysicalAnalyticsJob> {
    reddb_file::decode_physical_analytics_job_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical analytics job: {err}")))
}

pub(super) fn tree_definition_to_json(definition: &PhysicalTreeDefinition) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_tree_definition_json(definition)
            .expect("reddb-file must encode physical tree definition JSON"),
    )
}

pub(super) fn tree_definition_from_json(value: &JsonValue) -> io::Result<PhysicalTreeDefinition> {
    reddb_file::decode_physical_tree_definition_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical tree definition: {err}")))
}

pub(super) fn hypertable_chunk_to_json(chunk: &PhysicalHypertableChunk) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_persisted_physical_hypertable_chunk_json(
            &hypertable_chunk_to_persisted(chunk),
        )
        .expect("reddb-file must encode physical hypertable chunk JSON"),
    )
}

pub(super) fn hypertable_chunk_from_json(value: &JsonValue) -> io::Result<PhysicalHypertableChunk> {
    let persisted =
        reddb_file::decode_persisted_physical_hypertable_chunk_json(&value.to_string_compact())
            .map_err(|err| invalid_data(format!("decode physical hypertable chunk: {err}")))?;
    Ok(hypertable_chunk_from_persisted(persisted))
}

pub(super) fn hypertable_to_json(hypertable: &PhysicalHypertable) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_persisted_physical_hypertable_json(&hypertable_to_persisted(hypertable))
            .expect("reddb-file must encode physical hypertable JSON"),
    )
}

pub(super) fn hypertable_from_json(value: &JsonValue) -> io::Result<PhysicalHypertable> {
    let persisted =
        reddb_file::decode_persisted_physical_hypertable_json(&value.to_string_compact())
            .map_err(|err| invalid_data(format!("decode physical hypertable: {err}")))?;
    Ok(hypertable_from_persisted(persisted))
}

fn hypertable_to_persisted(
    hypertable: &PhysicalHypertable,
) -> reddb_file::PersistedPhysicalHypertable {
    reddb_file::PersistedPhysicalHypertable {
        name: hypertable.name.clone(),
        time_column: hypertable.time_column.clone(),
        chunk_interval_ns: hypertable.chunk_interval_ns,
        default_ttl_ns: hypertable.default_ttl_ns,
        chunks: hypertable
            .chunks
            .iter()
            .map(hypertable_chunk_to_persisted)
            .collect(),
    }
}

fn hypertable_from_persisted(
    hypertable: reddb_file::PersistedPhysicalHypertable,
) -> PhysicalHypertable {
    PhysicalHypertable {
        name: hypertable.name,
        time_column: hypertable.time_column,
        chunk_interval_ns: hypertable.chunk_interval_ns,
        default_ttl_ns: hypertable.default_ttl_ns,
        chunks: hypertable
            .chunks
            .into_iter()
            .map(hypertable_chunk_from_persisted)
            .collect(),
    }
}

fn hypertable_chunk_to_persisted(
    chunk: &PhysicalHypertableChunk,
) -> reddb_file::PersistedPhysicalHypertableChunk {
    reddb_file::PersistedPhysicalHypertableChunk {
        start_ns: chunk.start_ns,
        end_ns_exclusive: chunk.end_ns_exclusive,
        row_count: chunk.row_count,
        min_ts_ns: chunk.min_ts_ns,
        max_ts_ns: chunk.max_ts_ns,
        sealed: chunk.sealed,
        ttl_override_ns: chunk.ttl_override_ns,
        columnar_page: chunk
            .columnar_page
            .map(|loc| reddb_file::PhysicalPageLocation {
                page_id: loc.page_id,
                offset: loc.offset,
                length: loc.length,
            }),
    }
}

fn hypertable_chunk_from_persisted(
    chunk: reddb_file::PersistedPhysicalHypertableChunk,
) -> PhysicalHypertableChunk {
    PhysicalHypertableChunk {
        start_ns: chunk.start_ns,
        end_ns_exclusive: chunk.end_ns_exclusive,
        row_count: chunk.row_count,
        min_ts_ns: chunk.min_ts_ns,
        max_ts_ns: chunk.max_ts_ns,
        sealed: chunk.sealed,
        ttl_override_ns: chunk.ttl_override_ns,
        columnar_page: chunk
            .columnar_page
            .map(|loc| crate::storage::engine::PageLocation {
                page_id: loc.page_id,
                offset: loc.offset,
                length: loc.length,
            }),
    }
}

pub(super) fn index_kind_from_str(value: &str) -> io::Result<IndexKind> {
    match value {
        "btree" => Ok(IndexKind::BTree),
        "vector.hnsw" => Ok(IndexKind::VectorHnsw),
        "vector.inverted" => Ok(IndexKind::VectorInverted),
        "vector.turbo" => Ok(IndexKind::VectorTurbo),
        "graph.adjacency" => Ok(IndexKind::GraphAdjacency),
        "text.fulltext" => Ok(IndexKind::FullText),
        "document.pathvalue" => Ok(IndexKind::DocumentPathValue),
        "search.hybrid" => Ok(IndexKind::HybridSearch),
        other => Err(invalid_data(format!("unsupported index kind '{other}'"))),
    }
}

pub(super) fn export_descriptor_to_json(export: &ExportDescriptor) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_export_descriptor_json(export)
            .expect("reddb-file must encode physical export descriptor JSON"),
    )
}

pub(super) fn export_descriptor_from_json(value: &JsonValue) -> io::Result<ExportDescriptor> {
    reddb_file::decode_physical_export_descriptor_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical export descriptor: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_contract_object(with_ctx_enabled: Option<bool>) -> JsonValue {
        let mut obj = Map::new();
        obj.insert("name".to_string(), JsonValue::String("docs".to_string()));
        obj.insert(
            "declared_model".to_string(),
            JsonValue::String("table".to_string()),
        );
        obj.insert(
            "schema_mode".to_string(),
            JsonValue::String("dynamic".to_string()),
        );
        obj.insert(
            "origin".to_string(),
            JsonValue::String("explicit".to_string()),
        );
        obj.insert("version".to_string(), JsonValue::Number(1.0));
        obj.insert("created_at_unix_ms".to_string(), JsonValue::Number(0.0));
        obj.insert("updated_at_unix_ms".to_string(), JsonValue::Number(0.0));
        obj.insert("default_ttl_ms".to_string(), JsonValue::Null);
        obj.insert("table_def".to_string(), JsonValue::Null);
        obj.insert(
            "context_index_fields".to_string(),
            JsonValue::Array(Vec::new()),
        );
        obj.insert("declared_columns".to_string(), JsonValue::Array(Vec::new()));
        obj.insert("timestamps_enabled".to_string(), JsonValue::Bool(false));
        if let Some(v) = with_ctx_enabled {
            obj.insert("context_index_enabled".to_string(), JsonValue::Bool(v));
        }
        JsonValue::Object(obj)
    }

    #[test]
    fn legacy_sidecar_missing_context_index_defaults_true() {
        let value = minimal_contract_object(None);
        let contract = collection_contract_from_json(&value).expect("decode");
        assert!(
            contract.context_index_enabled,
            "missing key must default to true to preserve pre-PR behavior on upgrade"
        );
    }

    #[test]
    fn explicit_context_index_false_is_respected() {
        let value = minimal_contract_object(Some(false));
        let contract = collection_contract_from_json(&value).expect("decode");
        assert!(!contract.context_index_enabled);
    }

    #[test]
    fn explicit_context_index_true_is_respected() {
        let value = minimal_contract_object(Some(true));
        let contract = collection_contract_from_json(&value).expect("decode");
        assert!(contract.context_index_enabled);
    }

    #[test]
    fn columnar_page_round_trips_through_chunk_json() {
        // The migration discriminant MUST survive the sidecar (PRD #850).
        let chunk = PhysicalHypertableChunk {
            start_ns: 10,
            end_ns_exclusive: 20,
            row_count: 3,
            min_ts_ns: 11,
            max_ts_ns: 19,
            sealed: true,
            ttl_override_ns: Some(99),
            columnar_page: Some(crate::storage::engine::PageLocation::new(7, 0, 1234)),
        };
        let decoded =
            hypertable_chunk_from_json(&hypertable_chunk_to_json(&chunk)).expect("decode");
        assert_eq!(
            decoded.columnar_page,
            Some(crate::storage::engine::PageLocation::new(7, 0, 1234))
        );
        assert!(decoded.sealed);
        assert_eq!(decoded.ttl_override_ns, Some(99));
    }

    #[test]
    fn legacy_chunk_without_columnar_page_decodes_none() {
        // A sidecar written before the feature lacks the key — a
        // row-stored chunk must never be mis-read as columnar.
        let mut obj = Map::new();
        obj.insert("start_ns".to_string(), JsonValue::Number(0.0));
        obj.insert("end_ns_exclusive".to_string(), JsonValue::Number(1.0));
        obj.insert("row_count".to_string(), JsonValue::Number(0.0));
        obj.insert("min_ts_ns".to_string(), JsonValue::Number(0.0));
        obj.insert("max_ts_ns".to_string(), JsonValue::Number(0.0));
        let decoded = hypertable_chunk_from_json(&JsonValue::Object(obj)).expect("decode");
        assert_eq!(decoded.columnar_page, None);
    }

    #[test]
    fn analytical_storage_round_trips_and_defaults_none() {
        // Absent key (legacy sidecar) → row engine (None).
        let contract = collection_contract_from_json(&minimal_contract_object(None)).expect("dec");
        assert!(contract.analytical_storage.is_none());

        // Present + columnar → preserved verbatim.
        let JsonValue::Object(mut obj) = minimal_contract_object(None) else {
            unreachable!()
        };
        let mut cfg = Map::new();
        cfg.insert("columnar".to_string(), JsonValue::Bool(true));
        cfg.insert("time_key".to_string(), JsonValue::String("ts".to_string()));
        cfg.insert(
            "order_by_key".to_string(),
            JsonValue::String("host".to_string()),
        );
        obj.insert("analytical_storage".to_string(), JsonValue::Object(cfg));
        let contract = collection_contract_from_json(&JsonValue::Object(obj)).expect("dec");
        let cfg = contract.analytical_storage.expect("present");
        assert!(cfg.columnar);
        assert_eq!(cfg.time_key, "ts");
        assert_eq!(cfg.order_by_key.as_deref(), Some("host"));
    }
}
