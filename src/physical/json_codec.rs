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
        "table_def".to_string(),
        contract
            .table_def
            .as_ref()
            .map(|table_def| JsonValue::String(hex::encode(table_def.to_bytes())))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
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
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
    }
}

fn collection_model_from_str(value: &str) -> io::Result<crate::catalog::CollectionModel> {
    match value {
        "table" => Ok(crate::catalog::CollectionModel::Table),
        "document" => Ok(crate::catalog::CollectionModel::Document),
        "graph" => Ok(crate::catalog::CollectionModel::Graph),
        "vector" => Ok(crate::catalog::CollectionModel::Vector),
        "mixed" => Ok(crate::catalog::CollectionModel::Mixed),
        "timeseries" => Ok(crate::catalog::CollectionModel::TimeSeries),
        "queue" => Ok(crate::catalog::CollectionModel::Queue),
        other => Err(invalid_data(format!(
            "unsupported collection contract model: {other}"
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
    let mut collection_roots = Map::new();
    for (name, root) in &superblock.collection_roots {
        collection_roots.insert(name.clone(), json_u64(*root));
    }

    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(superblock.format_version as f64),
    );
    object.insert("sequence".to_string(), json_u64(superblock.sequence));
    object.insert(
        "copies".to_string(),
        JsonValue::Number(superblock.copies as f64),
    );
    object.insert(
        "manifest".to_string(),
        manifest_pointers_to_json(&superblock.manifest),
    );
    object.insert(
        "free_set".to_string(),
        block_reference_to_json(superblock.free_set),
    );
    object.insert(
        "collection_roots".to_string(),
        JsonValue::Object(collection_roots),
    );
    JsonValue::Object(object)
}

pub(super) fn superblock_from_json(value: &JsonValue) -> io::Result<SuperblockHeader> {
    let object = expect_object(value, "superblock")?;
    let roots = expect_object(
        json_required(object, "collection_roots")?,
        "superblock.roots",
    )?;
    let mut collection_roots = BTreeMap::new();
    for (name, root) in roots {
        collection_roots.insert(name.clone(), json_u64_value(root)?);
    }

    Ok(SuperblockHeader {
        format_version: json_u32_required(object, "format_version")?,
        sequence: json_u64_required(object, "sequence")?,
        copies: json_u8_required(object, "copies")?,
        manifest: manifest_pointers_from_json(json_required(object, "manifest")?)?,
        free_set: block_reference_from_json(json_required(object, "free_set")?)?,
        collection_roots,
    })
}

pub(super) fn manifest_event_to_json(event: &ManifestEvent) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(event.collection.clone()),
    );
    object.insert(
        "object_key".to_string(),
        JsonValue::String(event.object_key.clone()),
    );
    object.insert(
        "kind".to_string(),
        JsonValue::String(
            match event.kind {
                ManifestEventKind::Insert => "insert",
                ManifestEventKind::Update => "update",
                ManifestEventKind::Remove => "remove",
                ManifestEventKind::Checkpoint => "checkpoint",
            }
            .to_string(),
        ),
    );
    object.insert("block".to_string(), block_reference_to_json(event.block));
    object.insert("snapshot_min".to_string(), json_u64(event.snapshot_min));
    object.insert(
        "snapshot_max".to_string(),
        match event.snapshot_max {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(super) fn manifest_event_from_json(value: &JsonValue) -> io::Result<ManifestEvent> {
    let object = expect_object(value, "manifest event")?;
    Ok(ManifestEvent {
        collection: json_string_required(object, "collection")?,
        object_key: json_string_required(object, "object_key")?,
        kind: match json_string_required(object, "kind")?.as_str() {
            "insert" => ManifestEventKind::Insert,
            "update" => ManifestEventKind::Update,
            "remove" => ManifestEventKind::Remove,
            "checkpoint" => ManifestEventKind::Checkpoint,
            other => {
                return Err(invalid_data(format!(
                    "unsupported manifest event kind '{other}'"
                )))
            }
        },
        block: block_reference_from_json(json_required(object, "block")?)?,
        snapshot_min: json_u64_required(object, "snapshot_min")?,
        snapshot_max: object
            .get("snapshot_max")
            .and_then(|value| json_u64_value(value).ok()),
    })
}

pub(super) fn manifest_pointers_to_json(pointers: &ManifestPointers) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "oldest".to_string(),
        block_reference_to_json(pointers.oldest),
    );
    object.insert(
        "newest".to_string(),
        block_reference_to_json(pointers.newest),
    );
    JsonValue::Object(object)
}

pub(super) fn manifest_pointers_from_json(value: &JsonValue) -> io::Result<ManifestPointers> {
    let object = expect_object(value, "manifest pointers")?;
    Ok(ManifestPointers {
        oldest: block_reference_from_json(json_required(object, "oldest")?)?,
        newest: block_reference_from_json(json_required(object, "newest")?)?,
    })
}

pub(super) fn block_reference_to_json(reference: BlockReference) -> JsonValue {
    let mut object = Map::new();
    object.insert("index".to_string(), json_u64(reference.index));
    object.insert("checksum".to_string(), json_u128(reference.checksum));
    JsonValue::Object(object)
}

pub(super) fn block_reference_from_json(value: &JsonValue) -> io::Result<BlockReference> {
    let object = expect_object(value, "block reference")?;
    Ok(BlockReference {
        index: json_u64_required(object, "index")?,
        checksum: json_u128_required(object, "checksum")?,
    })
}

pub(super) fn snapshot_descriptor_to_json(snapshot: &SnapshotDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert("snapshot_id".to_string(), json_u64(snapshot.snapshot_id));
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(snapshot.created_at_unix_ms),
    );
    object.insert(
        "superblock_sequence".to_string(),
        json_u64(snapshot.superblock_sequence),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(snapshot.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(snapshot.total_entities as f64),
    );
    JsonValue::Object(object)
}

pub(super) fn snapshot_descriptor_from_json(value: &JsonValue) -> io::Result<SnapshotDescriptor> {
    let object = expect_object(value, "snapshot descriptor")?;
    Ok(SnapshotDescriptor {
        snapshot_id: json_u64_required(object, "snapshot_id")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}

pub(super) fn index_state_to_json(index: &PhysicalIndexState) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(index.name.clone()));
    object.insert(
        "kind".to_string(),
        JsonValue::String(index.kind.as_str().to_string()),
    );
    object.insert(
        "collection".to_string(),
        match &index.collection {
            Some(collection) => JsonValue::String(collection.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert("enabled".to_string(), JsonValue::Bool(index.enabled));
    object.insert(
        "entries".to_string(),
        JsonValue::Number(index.entries as f64),
    );
    object.insert(
        "estimated_memory_bytes".to_string(),
        json_u64(index.estimated_memory_bytes),
    );
    object.insert(
        "last_refresh_ms".to_string(),
        match index.last_refresh_ms {
            Some(value) => json_u128(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "backend".to_string(),
        JsonValue::String(index.backend.clone()),
    );
    object.insert(
        "artifact_kind".to_string(),
        match &index.artifact_kind {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "artifact_root_page".to_string(),
        match index.artifact_root_page {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "artifact_checksum".to_string(),
        match index.artifact_checksum {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "build_state".to_string(),
        JsonValue::String(index.build_state.clone()),
    );
    JsonValue::Object(object)
}

pub(super) fn index_state_from_json(value: &JsonValue) -> io::Result<PhysicalIndexState> {
    let object = expect_object(value, "physical index state")?;
    Ok(PhysicalIndexState {
        name: json_string_required(object, "name")?,
        kind: index_kind_from_str(&json_string_required(object, "kind")?)?,
        collection: object
            .get("collection")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        enabled: json_bool_required(object, "enabled")?,
        entries: json_usize_required(object, "entries")?,
        estimated_memory_bytes: json_u64_required(object, "estimated_memory_bytes")?,
        last_refresh_ms: object
            .get("last_refresh_ms")
            .and_then(|value| json_u128_value(value).ok()),
        backend: json_string_required(object, "backend")?,
        artifact_kind: object
            .get("artifact_kind")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        artifact_root_page: object
            .get("artifact_root_page")
            .and_then(JsonValue::as_u64)
            .map(|value| value as u32),
        artifact_checksum: object
            .get("artifact_checksum")
            .and_then(|value| json_u64_value(value).ok()),
        build_state: object
            .get("build_state")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

pub(super) fn graph_projection_to_json(projection: &PhysicalGraphProjection) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(projection.name.clone()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(projection.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(projection.updated_at_unix_ms),
    );
    object.insert(
        "state".to_string(),
        JsonValue::String(projection.state.clone()),
    );
    object.insert(
        "source".to_string(),
        JsonValue::String(projection.source.clone()),
    );
    object.insert(
        "node_labels".to_string(),
        JsonValue::Array(
            projection
                .node_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "node_types".to_string(),
        JsonValue::Array(
            projection
                .node_types
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "edge_labels".to_string(),
        JsonValue::Array(
            projection
                .edge_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        match projection.last_materialized_sequence {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(super) fn graph_projection_from_json(value: &JsonValue) -> io::Result<PhysicalGraphProjection> {
    let object = expect_object(value, "graph projection")?;
    Ok(PhysicalGraphProjection {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        state: object
            .get("state")
            .and_then(JsonValue::as_str)
            .unwrap_or("declared")
            .to_string(),
        source: json_string_required(object, "source")?,
        node_labels: object
            .get("node_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        node_types: object
            .get("node_types")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        edge_labels: object
            .get("edge_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        last_materialized_sequence: object
            .get("last_materialized_sequence")
            .and_then(|value| json_u64_value(value).ok()),
    })
}

pub(super) fn analytics_job_to_json(job: &PhysicalAnalyticsJob) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(job.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
    object.insert("state".to_string(), JsonValue::String(job.state.clone()));
    object.insert(
        "projection".to_string(),
        match &job.projection {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(job.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(job.updated_at_unix_ms),
    );
    object.insert(
        "last_run_sequence".to_string(),
        match job.last_run_sequence {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        JsonValue::Object(
            job.metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(super) fn analytics_job_from_json(value: &JsonValue) -> io::Result<PhysicalAnalyticsJob> {
    let object = expect_object(value, "analytics job")?;
    Ok(PhysicalAnalyticsJob {
        id: json_string_required(object, "id")?,
        kind: json_string_required(object, "kind")?,
        state: json_string_required(object, "state")?,
        projection: object
            .get("projection")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        last_run_sequence: object
            .get("last_run_sequence")
            .and_then(|value| json_u64_value(value).ok()),
        metadata: object
            .get("metadata")
            .and_then(JsonValue::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub(super) fn index_kind_from_str(value: &str) -> io::Result<IndexKind> {
    match value {
        "btree" => Ok(IndexKind::BTree),
        "vector.hnsw" => Ok(IndexKind::VectorHnsw),
        "vector.inverted" => Ok(IndexKind::VectorInverted),
        "graph.adjacency" => Ok(IndexKind::GraphAdjacency),
        "text.fulltext" => Ok(IndexKind::FullText),
        "document.pathvalue" => Ok(IndexKind::DocumentPathValue),
        "search.hybrid" => Ok(IndexKind::HybridSearch),
        other => Err(invalid_data(format!("unsupported index kind '{other}'"))),
    }
}

pub(super) fn export_descriptor_to_json(export: &ExportDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(export.name.clone()));
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(export.created_at_unix_ms),
    );
    object.insert(
        "snapshot_id".to_string(),
        match export.snapshot_id {
            Some(snapshot_id) => json_u64(snapshot_id),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "superblock_sequence".to_string(),
        json_u64(export.superblock_sequence),
    );
    object.insert(
        "data_path".to_string(),
        JsonValue::String(export.data_path.clone()),
    );
    object.insert(
        "metadata_path".to_string(),
        JsonValue::String(export.metadata_path.clone()),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(export.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(export.total_entities as f64),
    );
    JsonValue::Object(object)
}

pub(super) fn export_descriptor_from_json(value: &JsonValue) -> io::Result<ExportDescriptor> {
    let object = expect_object(value, "export descriptor")?;
    Ok(ExportDescriptor {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        snapshot_id: object
            .get("snapshot_id")
            .and_then(|value| json_u64_value(value).ok()),
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        data_path: json_string_required(object, "data_path")?,
        metadata_path: json_string_required(object, "metadata_path")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}
