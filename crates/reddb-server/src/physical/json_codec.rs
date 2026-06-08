use super::*;

pub(super) fn manifest_to_json(manifest: &SchemaManifest) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_schema_manifest_json(&schema_manifest_to_persisted(manifest))
            .expect("reddb-file must encode physical schema manifest JSON"),
    )
}

pub(super) fn manifest_from_json(value: &JsonValue) -> io::Result<SchemaManifest> {
    let persisted = reddb_file::decode_physical_schema_manifest_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical schema manifest: {err}")))?;
    schema_manifest_from_persisted(persisted)
}

fn schema_manifest_to_persisted(manifest: &SchemaManifest) -> reddb_file::PhysicalSchemaManifest {
    reddb_file::PhysicalSchemaManifest {
        format_version: manifest.format_version,
        created_at_unix_ms: manifest.created_at_unix_ms,
        updated_at_unix_ms: manifest.updated_at_unix_ms,
        collection_count: manifest.collection_count,
        options: reddb_file::PhysicalSchemaOptions {
            mode: match manifest.options.mode {
                StorageMode::Persistent => "persistent".to_string(),
            },
            data_path: manifest
                .options
                .data_path
                .as_ref()
                .map(|path| path.display().to_string()),
            read_only: manifest.options.read_only,
            create_if_missing: manifest.options.create_if_missing,
            verify_checksums: manifest.options.verify_checksums,
            durability_mode: Some(manifest.options.durability_mode.as_str().to_string()),
            group_commit_window_ms: Some(manifest.options.group_commit.window_ms),
            group_commit_max_statements: Some(manifest.options.group_commit.max_statements),
            group_commit_max_wal_bytes: Some(manifest.options.group_commit.max_wal_bytes),
            auto_checkpoint_pages: manifest.options.auto_checkpoint_pages,
            cache_pages: manifest.options.cache_pages,
            snapshot_retention: Some(manifest.options.snapshot_retention),
            export_retention: Some(manifest.options.export_retention),
            force_create: manifest.options.force_create,
            capabilities: manifest
                .options
                .feature_gates
                .as_slice()
                .into_iter()
                .map(|capability| capability.as_str().to_string())
                .collect(),
            metadata: manifest.options.metadata.clone(),
        },
    }
}

fn schema_manifest_from_persisted(
    manifest: reddb_file::PhysicalSchemaManifest,
) -> io::Result<SchemaManifest> {
    let options = manifest.options;
    let mut runtime_options = RedDBOptions {
        mode: match options.mode.as_str() {
            "persistent" | "in_memory" => StorageMode::Persistent,
            other => {
                return Err(invalid_data(format!(
                    "unsupported storage mode in manifest: {other}"
                )))
            }
        },
        data_path: options.data_path.map(PathBuf::from),
        read_only: options.read_only,
        create_if_missing: options.create_if_missing,
        verify_checksums: options.verify_checksums,
        durability_mode: options
            .durability_mode
            .as_deref()
            .and_then(crate::api::DurabilityMode::from_str)
            .unwrap_or(crate::api::DurabilityMode::Strict),
        group_commit: crate::api::GroupCommitOptions {
            window_ms: options
                .group_commit_window_ms
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_WINDOW_MS)
                .max(1),
            max_statements: options
                .group_commit_max_statements
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_MAX_STATEMENTS)
                .max(1),
            max_wal_bytes: options
                .group_commit_max_wal_bytes
                .unwrap_or(crate::api::DEFAULT_GROUP_COMMIT_MAX_WAL_BYTES)
                .max(1),
        },
        auto_checkpoint_pages: options.auto_checkpoint_pages,
        cache_pages: options.cache_pages,
        snapshot_retention: options
            .snapshot_retention
            .unwrap_or(crate::api::DEFAULT_SNAPSHOT_RETENTION)
            .max(1),
        export_retention: options
            .export_retention
            .unwrap_or(crate::api::DEFAULT_EXPORT_RETENTION)
            .max(1),
        force_create: options.force_create,
        metadata: options.metadata,
        ..Default::default()
    };
    runtime_options.feature_gates =
        options
            .capabilities
            .iter()
            .fold(Default::default(), |set, value| match value.as_str() {
                "table" => set.with(crate::api::Capability::Table),
                "graph" => set.with(crate::api::Capability::Graph),
                "vector" => set.with(crate::api::Capability::Vector),
                "fulltext" => set.with(crate::api::Capability::FullText),
                "security" => set.with(crate::api::Capability::Security),
                "encryption" => set.with(crate::api::Capability::Encryption),
                _ => set,
            });
    Ok(SchemaManifest {
        format_version: manifest.format_version,
        created_at_unix_ms: manifest.created_at_unix_ms,
        updated_at_unix_ms: manifest.updated_at_unix_ms,
        options: runtime_options,
        collection_count: manifest.collection_count,
    })
}

pub(super) fn catalog_to_json(catalog: &CatalogSnapshot) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_catalog_snapshot_json(&catalog_to_persisted(catalog))
            .expect("reddb-file must encode physical catalog snapshot JSON"),
    )
}

pub(super) fn catalog_from_json(value: &JsonValue) -> io::Result<CatalogSnapshot> {
    let persisted =
        reddb_file::decode_physical_catalog_snapshot_json(&value.to_string_compact())
            .map_err(|err| invalid_data(format!("decode physical catalog snapshot: {err}")))?;
    catalog_from_persisted(persisted)
}

fn catalog_to_persisted(catalog: &CatalogSnapshot) -> reddb_file::PhysicalCatalogSnapshot {
    let mut stats_by_collection = BTreeMap::new();
    for (name, stat) in &catalog.stats_by_collection {
        stats_by_collection.insert(
            name.clone(),
            reddb_file::PhysicalCatalogCollectionStats {
                entities: stat.entities,
                cross_refs: stat.cross_refs,
                segments: stat.segments,
            },
        );
    }

    reddb_file::PhysicalCatalogSnapshot {
        name: catalog.name.clone(),
        total_entities: catalog.total_entities,
        total_collections: catalog.total_collections,
        updated_at_unix_ms: catalog
            .updated_at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        stats_by_collection,
    }
}

fn catalog_from_persisted(
    catalog: reddb_file::PhysicalCatalogSnapshot,
) -> io::Result<CatalogSnapshot> {
    let stats_by_collection = catalog
        .stats_by_collection
        .into_iter()
        .map(|(name, stat)| {
            (
                name,
                CollectionStats {
                    entities: stat.entities,
                    cross_refs: stat.cross_refs,
                    segments: stat.segments,
                },
            )
        })
        .collect();

    Ok(CatalogSnapshot {
        name: catalog.name,
        total_entities: catalog.total_entities,
        total_collections: catalog.total_collections,
        stats_by_collection,
        updated_at: UNIX_EPOCH
            + std::time::Duration::from_millis(
                catalog.updated_at_unix_ms.try_into().unwrap_or(u64::MAX),
            ),
    })
}

pub(super) fn collection_contract_to_json(contract: &CollectionContract) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_collection_contract_json(&collection_contract_to_persisted(
            contract,
        ))
        .expect("reddb-file must encode physical collection contract JSON"),
    )
}

fn collection_contract_to_persisted(
    contract: &CollectionContract,
) -> reddb_file::PhysicalCollectionContract {
    reddb_file::PhysicalCollectionContract {
        name: contract.name.clone(),
        declared_model: collection_model_as_str(contract.declared_model).to_string(),
        schema_mode: schema_mode_as_str(contract.schema_mode).to_string(),
        origin: contract.origin.as_str().to_string(),
        version: contract.version,
        created_at_unix_ms: contract.created_at_unix_ms,
        updated_at_unix_ms: contract.updated_at_unix_ms,
        default_ttl_ms: contract.default_ttl_ms,
        vector_dimension: contract.vector_dimension,
        vector_metric: contract
            .vector_metric
            .map(distance_metric_as_str)
            .map(str::to_string),
        context_index_fields: contract.context_index_fields.clone(),
        declared_columns: contract
            .declared_columns
            .iter()
            .map(declared_column_contract_to_persisted)
            .collect(),
        table_def_hex: contract
            .table_def
            .as_ref()
            .map(|table_def| hex::encode(table_def.to_bytes())),
        timestamps_enabled: contract.timestamps_enabled,
        context_index_enabled: contract.context_index_enabled,
        metrics_raw_retention_ms: contract.metrics_raw_retention_ms,
        metrics_rollup_policies: contract.metrics_rollup_policies.clone(),
        metrics_tenant_identity: contract.metrics_tenant_identity.clone(),
        metrics_namespace: contract.metrics_namespace.clone(),
        append_only: contract.append_only,
        subscriptions: contract
            .subscriptions
            .iter()
            .map(subscription_descriptor_to_persisted)
            .collect(),
        analytics_config: contract
            .analytics_config
            .iter()
            .map(analytics_view_descriptor_to_persisted)
            .collect(),
        session_key: contract.session_key.clone(),
        session_gap_ms: contract.session_gap_ms,
        retention_duration_ms: contract.retention_duration_ms,
        analytical_storage: contract
            .analytical_storage
            .as_ref()
            .map(analytical_storage_to_persisted),
    }
}

fn collection_contract_from_persisted(
    contract: reddb_file::PhysicalCollectionContract,
) -> io::Result<CollectionContract> {
    let table_def = match contract.table_def_hex {
        Some(encoded) => {
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
        None => None,
    };

    Ok(CollectionContract {
        name: contract.name,
        declared_model: collection_model_from_str(&contract.declared_model)?,
        schema_mode: schema_mode_from_str(&contract.schema_mode)?,
        origin: contract_origin_from_str(&contract.origin)?,
        version: contract.version,
        created_at_unix_ms: contract.created_at_unix_ms,
        updated_at_unix_ms: contract.updated_at_unix_ms,
        default_ttl_ms: contract.default_ttl_ms,
        vector_dimension: contract.vector_dimension,
        vector_metric: contract
            .vector_metric
            .as_deref()
            .map(distance_metric_from_str)
            .transpose()?,
        context_index_fields: contract.context_index_fields,
        declared_columns: contract
            .declared_columns
            .into_iter()
            .map(declared_column_contract_from_persisted)
            .collect(),
        table_def,
        timestamps_enabled: contract.timestamps_enabled,
        context_index_enabled: contract.context_index_enabled,
        metrics_raw_retention_ms: contract.metrics_raw_retention_ms,
        metrics_rollup_policies: contract.metrics_rollup_policies,
        metrics_tenant_identity: contract.metrics_tenant_identity,
        metrics_namespace: contract.metrics_namespace,
        append_only: contract.append_only,
        subscriptions: contract
            .subscriptions
            .into_iter()
            .map(subscription_descriptor_from_persisted)
            .collect::<io::Result<Vec<_>>>()?,
        analytics_config: contract
            .analytics_config
            .into_iter()
            .map(analytics_view_descriptor_from_persisted)
            .collect::<io::Result<Vec<_>>>()?,
        session_key: contract.session_key,
        session_gap_ms: contract.session_gap_ms,
        retention_duration_ms: contract.retention_duration_ms,
        analytical_storage: contract
            .analytical_storage
            .map(analytical_storage_from_persisted),
    })
}

fn analytical_storage_to_json(cfg: &crate::catalog::AnalyticalStorageConfig) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_analytical_storage_json(&analytical_storage_to_persisted(cfg))
            .expect("reddb-file must encode physical analytical storage JSON"),
    )
}

fn analytical_storage_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticalStorageConfig> {
    let persisted = reddb_file::decode_physical_analytical_storage_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical analytical storage: {err}")))?;
    Ok(analytical_storage_from_persisted(persisted))
}

fn analytical_storage_to_persisted(
    cfg: &crate::catalog::AnalyticalStorageConfig,
) -> reddb_file::PhysicalAnalyticalStorageConfig {
    reddb_file::PhysicalAnalyticalStorageConfig {
        columnar: cfg.columnar,
        time_key: cfg.time_key.clone(),
        order_by_key: cfg.order_by_key.clone(),
    }
}

fn analytical_storage_from_persisted(
    cfg: reddb_file::PhysicalAnalyticalStorageConfig,
) -> crate::catalog::AnalyticalStorageConfig {
    crate::catalog::AnalyticalStorageConfig {
        columnar: cfg.columnar,
        time_key: cfg.time_key,
        order_by_key: cfg.order_by_key,
    }
}

pub(super) fn collection_contract_from_json(value: &JsonValue) -> io::Result<CollectionContract> {
    let contract = reddb_file::decode_physical_collection_contract_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical collection contract: {err}")))?;
    collection_contract_from_persisted(contract)
}

fn subscription_descriptor_to_json(
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_subscription_descriptor_json(
            &subscription_descriptor_to_persisted(subscription),
        )
        .expect("reddb-file must encode physical subscription descriptor JSON"),
    )
}

fn subscription_descriptor_to_persisted(
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> reddb_file::PhysicalSubscriptionDescriptor {
    reddb_file::PhysicalSubscriptionDescriptor {
        name: subscription.name.clone(),
        source: subscription.source.clone(),
        target_queue: subscription.target_queue.clone(),
        ops_filter: subscription
            .ops_filter
            .iter()
            .map(|op| op.as_str().to_string())
            .collect(),
        where_filter: subscription.where_filter.clone(),
        redact_fields: subscription.redact_fields.clone(),
        enabled: subscription.enabled,
        all_tenants: subscription.all_tenants,
    }
}

fn subscription_descriptor_from_persisted(
    subscription: reddb_file::PhysicalSubscriptionDescriptor,
) -> io::Result<crate::catalog::SubscriptionDescriptor> {
    let ops_filter = subscription
        .ops_filter
        .iter()
        .map(|op| {
            crate::catalog::SubscriptionOperation::from_str(op).ok_or_else(|| {
                invalid_data(format!(
                    "unsupported subscription operation in catalog: {op}"
                ))
            })
        })
        .collect::<io::Result<Vec<_>>>()?;

    Ok(crate::catalog::SubscriptionDescriptor {
        name: subscription.name,
        source: subscription.source,
        target_queue: subscription.target_queue,
        ops_filter,
        where_filter: subscription.where_filter,
        redact_fields: subscription.redact_fields,
        enabled: subscription.enabled,
        all_tenants: subscription.all_tenants,
    })
}

fn subscription_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::SubscriptionDescriptor> {
    let subscription =
        reddb_file::decode_physical_subscription_descriptor_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical subscription descriptor: {err}"))
            })?;
    subscription_descriptor_from_persisted(subscription)
}

fn analytics_view_descriptor_to_json(view: &crate::catalog::AnalyticsViewDescriptor) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_analytics_view_descriptor_json(
            &analytics_view_descriptor_to_persisted(view),
        )
        .expect("reddb-file must encode physical analytics view descriptor JSON"),
    )
}

fn analytics_view_descriptor_to_persisted(
    view: &crate::catalog::AnalyticsViewDescriptor,
) -> reddb_file::PhysicalAnalyticsViewDescriptor {
    reddb_file::PhysicalAnalyticsViewDescriptor {
        output: view.output.as_str().to_string(),
        algorithm: view.algorithm.clone(),
        resolution: view.resolution,
        max_iterations: view.max_iterations,
        tolerance: view.tolerance,
    }
}

fn analytics_view_descriptor_from_persisted(
    view: reddb_file::PhysicalAnalyticsViewDescriptor,
) -> io::Result<crate::catalog::AnalyticsViewDescriptor> {
    let output = crate::catalog::AnalyticsOutput::from_str(&view.output).ok_or_else(|| {
        invalid_data(format!(
            "analytics_view_descriptor.output has unsupported value: {}",
            view.output
        ))
    })?;
    Ok(crate::catalog::AnalyticsViewDescriptor {
        output,
        algorithm: view.algorithm,
        resolution: view.resolution,
        max_iterations: view.max_iterations,
        tolerance: view.tolerance,
    })
}

fn analytics_view_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticsViewDescriptor> {
    let view =
        reddb_file::decode_physical_analytics_view_descriptor_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical analytics view descriptor: {err}"))
            })?;
    analytics_view_descriptor_from_persisted(view)
}

fn declared_column_contract_to_json(column: &DeclaredColumnContract) -> JsonValue {
    file_json_to_server_json(
        reddb_file::encode_physical_declared_column_contract_json(
            &declared_column_contract_to_persisted(column),
        )
        .expect("reddb-file must encode physical declared column contract JSON"),
    )
}

fn declared_column_contract_to_persisted(
    column: &DeclaredColumnContract,
) -> reddb_file::PhysicalDeclaredColumnContract {
    reddb_file::PhysicalDeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: column.sql_type.as_ref().map(sql_type_name_to_persisted),
        not_null: column.not_null,
        default: column.default.clone(),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

fn declared_column_contract_from_persisted(
    column: reddb_file::PhysicalDeclaredColumnContract,
) -> DeclaredColumnContract {
    let sql_type = column
        .sql_type
        .map(sql_type_name_from_persisted)
        .or_else(|| {
            Some(crate::storage::schema::SqlTypeName::parse_declared(
                &column.data_type,
            ))
        });
    DeclaredColumnContract {
        name: column.name,
        data_type: column.data_type,
        sql_type,
        not_null: column.not_null,
        default: column.default,
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants,
        array_element: column.array_element,
        decimal_precision: column.decimal_precision,
    }
}

fn declared_column_contract_from_json(value: &JsonValue) -> io::Result<DeclaredColumnContract> {
    let column =
        reddb_file::decode_physical_declared_column_contract_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical declared column contract: {err}"))
            })?;
    Ok(declared_column_contract_from_persisted(column))
}

fn sql_type_name_to_persisted(
    sql_type: &crate::storage::schema::SqlTypeName,
) -> reddb_file::PhysicalSqlTypeName {
    reddb_file::PhysicalSqlTypeName {
        name: sql_type.name.clone(),
        modifiers: sql_type
            .modifiers
            .iter()
            .map(type_modifier_to_persisted)
            .collect(),
    }
}

fn sql_type_name_from_persisted(
    sql_type: reddb_file::PhysicalSqlTypeName,
) -> crate::storage::schema::SqlTypeName {
    crate::storage::schema::SqlTypeName {
        name: sql_type.name,
        modifiers: sql_type
            .modifiers
            .into_iter()
            .map(type_modifier_from_persisted)
            .collect(),
    }
}

fn type_modifier_to_persisted(
    modifier: &crate::storage::schema::TypeModifier,
) -> reddb_file::PhysicalTypeModifier {
    match modifier {
        crate::storage::schema::TypeModifier::Number(value) => {
            reddb_file::PhysicalTypeModifier::Number(*value)
        }
        crate::storage::schema::TypeModifier::Ident(value) => {
            reddb_file::PhysicalTypeModifier::Ident(value.clone())
        }
        crate::storage::schema::TypeModifier::StringLiteral(value) => {
            reddb_file::PhysicalTypeModifier::StringLiteral(value.clone())
        }
        crate::storage::schema::TypeModifier::Type(value) => {
            reddb_file::PhysicalTypeModifier::Type(Box::new(sql_type_name_to_persisted(value)))
        }
    }
}

fn type_modifier_from_persisted(
    modifier: reddb_file::PhysicalTypeModifier,
) -> crate::storage::schema::TypeModifier {
    match modifier {
        reddb_file::PhysicalTypeModifier::Number(value) => {
            crate::storage::schema::TypeModifier::Number(value)
        }
        reddb_file::PhysicalTypeModifier::Ident(value) => {
            crate::storage::schema::TypeModifier::Ident(value)
        }
        reddb_file::PhysicalTypeModifier::StringLiteral(value) => {
            crate::storage::schema::TypeModifier::StringLiteral(value)
        }
        reddb_file::PhysicalTypeModifier::Type(value) => {
            crate::storage::schema::TypeModifier::Type(Box::new(sql_type_name_from_persisted(
                *value,
            )))
        }
    }
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
