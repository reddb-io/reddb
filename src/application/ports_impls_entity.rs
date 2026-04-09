use crate::application::entity::{CreateDocumentInput, CreateKvInput};
use crate::json::{to_vec as json_to_vec, Value as JsonValue};
use crate::storage::unified::MetadataValue;

use super::*;

const RESERVED_TTL_METADATA_KEYS: [&str; 3] = ["_ttl", "_ttl_ms", "_expires_at"];

fn apply_collection_default_ttl(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    metadata: &mut Vec<(String, MetadataValue)>,
) {
    if metadata.iter().any(|(key, _)| {
        RESERVED_TTL_METADATA_KEYS
            .iter()
            .any(|reserved| key.eq_ignore_ascii_case(reserved))
    }) {
        return;
    }

    let Some(default_ttl_ms) = db.collection_default_ttl_ms(collection) else {
        return;
    };

    metadata.push((
        "_ttl_ms".to_string(),
        if default_ttl_ms <= i64::MAX as u64 {
            MetadataValue::Int(default_ttl_ms as i64)
        } else {
            MetadataValue::Timestamp(default_ttl_ms)
        },
    ));
}

impl RuntimeEntityPort for RedDBRuntime {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let columns: Vec<(&str, crate::storage::schema::Value)> = input
            .fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        let mut builder = db.row(&input.collection, columns);

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        for node in input.node_links {
            builder = builder.link_to_node(node);
        }

        for vector in input.vector_links {
            builder = builder.link_to_vector(vector);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db.node(&input.collection, &input.label);

        if let Some(node_type) = input.node_type {
            builder = builder.node_type(node_type);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        for embedding in input.embeddings {
            if let Some(model) = embedding.model {
                builder = builder.embedding_with_model(embedding.name, embedding.vector, model);
            } else {
                builder = builder.embedding(embedding.name, embedding.vector);
            }
        }

        for link in input.table_links {
            builder = builder.link_to_table(link.key, link.table);
        }

        for link in input.node_links {
            builder = builder.link_to_weighted(link.target, link.edge_label, link.weight);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db
            .edge(&input.collection, &input.label)
            .from(input.from)
            .to(input.to);

        if let Some(weight) = input.weight {
            builder = builder.weight(weight);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db.vector(&input.collection).dense(input.dense);

        if let Some(content) = input.content {
            builder = builder.content(content);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        if let Some(link_row) = input.link_row {
            builder = builder.link_to_table(link_row);
        }

        if let Some(link_node) = input.link_node {
            builder = builder.link_to_node(link_node);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_document(&self, input: CreateDocumentInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();

        // Serialize the full body as Value::Json for the "body" field
        let body_bytes = json_to_vec(&input.body).map_err(|err| {
            crate::RedDBError::Query(format!("failed to serialize document body: {err}"))
        })?;
        let mut fields: Vec<(String, crate::storage::schema::Value)> = vec![(
            "body".to_string(),
            crate::storage::schema::Value::Json(body_bytes),
        )];

        // Flatten top-level keys from the body into named fields for filtering
        if let JsonValue::Object(ref map) = input.body {
            for (key, value) in map {
                let storage_value = json_to_storage_value(value)?;
                fields.push((key.clone(), storage_value));
            }
        }

        let row_input = CreateRowInput {
            collection: input.collection,
            fields,
            metadata: input.metadata,
            node_links: input.node_links,
            vector_links: input.vector_links,
        };
        self.create_row(row_input)
    }

    fn create_kv(&self, input: CreateKvInput) -> RedDBResult<CreateEntityOutput> {
        let fields = vec![
            (
                "key".to_string(),
                crate::storage::schema::Value::Text(input.key),
            ),
            ("value".to_string(), input.value),
        ];
        let row_input = CreateRowInput {
            collection: input.collection,
            fields,
            metadata: input.metadata,
            node_links: Vec::new(),
            vector_links: Vec::new(),
        };
        self.create_row(row_input)
    }

    fn get_kv(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>> {
        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let entities = manager.query_all(|_| true);
        for entity in entities {
            if let crate::storage::EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    if let Some(crate::storage::schema::Value::Text(ref k)) = named.get("key") {
                        if k == key {
                            let value = named
                                .get("value")
                                .cloned()
                                .unwrap_or(crate::storage::schema::Value::Null);
                            return Ok(Some((value, entity.id)));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn delete_kv(&self, collection: &str, key: &str) -> RedDBResult<bool> {
        let found = self.get_kv(collection, key)?;
        if let Some((_, id)) = found {
            let db = self.db();
            db.store()
                .delete(collection, id)
                .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn patch_entity(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput> {
        let PatchEntityInput {
            collection,
            id,
            payload,
            operations,
        } = input;

        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };
        let Some(mut entity) = manager.get(id) else {
            return Err(crate::RedDBError::NotFound(format!(
                "entity not found: {}",
                id.raw()
            )));
        };

        let mut patch_metadata = store.get_metadata(&collection, id).unwrap_or_default();
        let mut metadata_changed = false;

        match &mut entity.data {
            crate::storage::EntityData::Row(row) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "named" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for table rows. Use fields/*, metadata/*, or weight"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    let named = row.named.get_or_insert_with(Default::default);
                    apply_patch_operations_to_storage_map(named, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    let named = row.named.get_or_insert_with(Default::default);
                    for (key, value) in fields {
                        named.insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Node(node) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph nodes. Use fields/*, properties/*, or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_storage_map(&mut node.properties, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    for (key, value) in fields {
                        node.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Edge(edge) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();
                let mut weight_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "weight" => {
                            if op.path.len() != 1 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'weight' does not allow nested keys".to_string(),
                                ));
                            }
                            op.path.clear();
                            weight_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph edges. Use fields/*, weight, metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_storage_map(&mut edge.properties, &field_ops)?;
                }

                for op in weight_ops {
                    let value = op.value.ok_or_else(|| {
                        crate::RedDBError::Query("weight operations require a value".to_string())
                    })?;

                    match op.op {
                        PatchEntityOperationType::Unset => {
                            return Err(crate::RedDBError::Query(
                                "weight cannot be unset through patch operations".to_string(),
                            ));
                        }
                        PatchEntityOperationType::Set | PatchEntityOperationType::Replace => {
                            let Some(weight) = value.as_f64() else {
                                return Err(crate::RedDBError::Query(
                                    "weight operation requires a numeric value".to_string(),
                                ));
                            };
                            edge.weight = weight as f32;
                        }
                    }
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    for (key, value) in fields {
                        edge.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Vector(vector) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            let Some(target) = op.path.first().map(String::as_str) else {
                                return Err(crate::RedDBError::Query(
                                    "patch path requires a target under fields".to_string(),
                                ));
                            };
                            if !matches!(target, "dense" | "content" | "sparse") {
                                return Err(crate::RedDBError::Query(format!(
                                    "unsupported vector patch target '{target}'"
                                )));
                            }
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for vectors. Use fields/* or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_vector_fields(vector, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    if let Some(content) =
                        fields.get("content").and_then(crate::json::Value::as_str)
                    {
                        vector.content = Some(content.to_string());
                    }
                    if let Some(dense) = fields.get("dense") {
                        vector.dense = dense
                            .as_array()
                            .ok_or_else(|| {
                                crate::RedDBError::Query(
                                    "field 'dense' must be an array".to_string(),
                                )
                            })?
                            .iter()
                            .map(|value| {
                                value.as_f64().map(|value| value as f32).ok_or_else(|| {
                                    crate::RedDBError::Query(
                                        "field 'dense' must contain only numbers".to_string(),
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
        }

        if let Some(metadata) = payload
            .get("metadata")
            .and_then(crate::json::Value::as_object)
        {
            for (key, value) in metadata {
                patch_metadata.set(key.clone(), json_to_metadata_value(value)?);
            }
            metadata_changed = true;
        }

        if metadata_changed {
            store
                .set_metadata(&collection, id, patch_metadata)
                .map_err(|err| crate::RedDBError::Query(err.to_string()))?;
        }

        entity.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        manager
            .update(entity)
            .map_err(|err| crate::RedDBError::Query(err.to_string()))?;

        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput> {
        let deleted = self
            .db()
            .store()
            .delete(&input.collection, input.id)
            .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
        Ok(DeleteEntityOutput {
            deleted,
            id: input.id,
        })
    }
}
