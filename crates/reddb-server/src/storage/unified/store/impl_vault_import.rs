use super::*;

// Bound expansion before the physical decoder allocates a TOAST value.
// Encrypted Vault values do not expand; this budget covers compressed row text.
const VAULT_IMPORT_EXPANSION_BYTES_MAX: usize = 16 * 1024 * 1024;

impl UnifiedStore {
    /// Validate external Vault records before using the trusted physical decoder.
    /// Counts and lengths are bounded by the payload, and nested values use the
    /// same depth limit as query literals. This does not change the storage format.
    pub(crate) fn deserialize_vault_entity_record(
        data: &[u8],
        format_version: u32,
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        if !is_supported_store_version(format_version) {
            return Err(vault_import_error(
                "unsupported native vault storage format",
            ));
        }
        let frame = reddb_file::decode_native_entity_record_frame(data)
            .map_err(|error| StoreError::Serialization(error.to_string()))?
            .ok_or_else(|| vault_import_error("vault dump requires a native record frame"))?;
        let frame_size = frame
            .entity
            .len()
            .checked_add(frame.metadata.len())
            .and_then(|size| size.checked_add(12));
        if frame_size != Some(data.len()) {
            return Err(vault_import_error("trailing native vault record bytes"));
        }
        let mut entity = VaultImportReader::new(frame.entity);
        entity.validate_entity(format_version)?;
        if !frame.metadata.is_empty() {
            let mut metadata = VaultImportReader::new(frame.metadata);
            let count = metadata.count_fixed()?;
            for _ in 0..count {
                metadata.bytes_fixed()?;
                metadata.validate_metadata_value(0)?;
            }
            metadata.finish()?;
        }
        Self::deserialize_entity_record(data, format_version)
    }
}

struct VaultImportReader<'a> {
    remaining: &'a [u8],
    expansion_bytes: usize,
}

impl<'a> VaultImportReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            remaining: data,
            expansion_bytes: 0,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], StoreError> {
        if count > self.remaining.len() {
            return Err(vault_import_error("truncated native vault record"));
        }
        let (value, remaining) = self.remaining.split_at(count);
        self.remaining = remaining;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, StoreError> {
        Ok(self.take(1)?[0])
    }

    fn variable(&mut self, bits: u32) -> Result<u64, StoreError> {
        let mut value = 0;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7f);
            if shift >= bits || payload > (u64::MAX >> (64 - bits)) >> shift {
                return Err(vault_import_error("native vault varint overflow"));
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn size_variable(&mut self) -> Result<usize, StoreError> {
        usize::try_from(self.variable(32)?)
            .map_err(|_| vault_import_error("native vault size overflow"))
    }

    fn size_fixed(&mut self) -> Result<usize, StoreError> {
        let bytes = self.take(4)?;
        let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        usize::try_from(count).map_err(|_| vault_import_error("native vault size overflow"))
    }

    fn count_variable(&mut self) -> Result<usize, StoreError> {
        let count = self.size_variable()?;
        self.check_count(count)
    }

    fn count_fixed(&mut self) -> Result<usize, StoreError> {
        let count = self.size_fixed()?;
        self.check_count(count)
    }

    fn check_count(&self, count: usize) -> Result<usize, StoreError> {
        if count > self.remaining.len() {
            return Err(vault_import_error("native vault count exceeds payload"));
        }
        Ok(count)
    }

    fn bytes_variable(&mut self) -> Result<(), StoreError> {
        let size = self.size_variable()?;
        self.take(size)?;
        Ok(())
    }

    fn bytes_fixed(&mut self) -> Result<(), StoreError> {
        let size = self.size_fixed()?;
        self.take(size)?;
        Ok(())
    }

    fn finish(&self) -> Result<(), StoreError> {
        if !self.remaining.is_empty() {
            return Err(vault_import_error("trailing native vault payload bytes"));
        }
        Ok(())
    }

    fn validate_entity(&mut self, format_version: u32) -> Result<(), StoreError> {
        self.variable(64)?;
        if self.byte()? != 0 {
            return Err(vault_import_error("vault dump requires table rows"));
        }
        self.bytes_variable()?;
        self.variable(64)?;
        let data_type = self.byte()?;
        if !matches!(data_type, 0 | 6) {
            return Err(vault_import_error("vault dump requires rows"));
        }
        let field_count = self.count_variable()?;
        for _ in 0..field_count {
            if data_type == 6 {
                self.bytes_variable()?;
            }
            self.validate_value(0)?;
        }
        self.variable(64)?; // created_at
        self.variable(64)?; // updated_at
        let embedding_count = self.count_variable()?;
        for _ in 0..embedding_count {
            self.bytes_variable()?;
            let dimension = self.size_variable()?;
            let size = dimension
                .checked_mul(4)
                .ok_or_else(|| vault_import_error("native vault vector size overflow"))?;
            self.take(size)?;
            self.bytes_variable()?;
        }
        let reference_count = self.count_variable()?;
        for _ in 0..reference_count {
            self.variable(64)?;
            self.variable(64)?;
            self.byte()?;
            if format_version >= STORE_VERSION_V2 {
                self.bytes_variable()?;
                self.take(4)?;
                self.variable(64)?;
            }
        }
        self.variable(64)?; // sequence_id
        if format_version >= STORE_VERSION_V8 && self.byte()? != 0 {
            self.variable(64)?;
        }
        if format_version >= STORE_VERSION_V9 {
            self.variable(64)?; // xmin
            self.variable(64)?; // xmax
        }
        self.finish()
    }

    fn validate_value(&mut self, depth: u32) -> Result<(), StoreError> {
        vault_import_depth(depth)?;
        match self.byte()? {
            0 => {}
            1 | 27 => {
                self.take(1)?;
            }
            2..=4 | 7 | 8 | 21 | 26 | 29 | 32 | 36 | 43 => {
                self.take(8)?;
            }
            5 | 6 | 12 | 14 | 15 | 19 | 20 | 46 | 48..=50 | 52 => {
                self.bytes_variable()?;
            }
            9 => {
                let version = self.byte()?;
                self.take(if version == 4 { 4 } else { 16 })?;
            }
            10 => {
                self.take(6)?;
            }
            11 => {
                let dimension = self.size_variable()?;
                let size = dimension
                    .checked_mul(4)
                    .ok_or_else(|| vault_import_error("native vault vector size overflow"))?;
                self.take(size)?;
            }
            13 | 31 => {
                self.take(16)?;
            }
            16 | 17 | 45 => {
                self.bytes_variable()?;
                self.take(8)?;
            }
            18 | 38 | 41 => {
                self.take(3)?;
            }
            22 | 24 | 25 | 30 | 34 | 35 | 42 | 47 => {
                self.take(4)?;
            }
            23 | 40 => {
                self.take(5)?;
            }
            28 => {
                let count = self.count_variable()?;
                for _ in 0..count {
                    self.validate_value(depth + 1)?;
                }
            }
            33 | 37 | 39 => {
                self.take(2)?;
            }
            44 => {
                self.bytes_variable()?;
                self.bytes_variable()?;
            }
            51 => {
                self.bytes_variable()?;
                self.take(9)?;
            }
            0x85 | 0x86 => {
                let original_size = self.size_variable()?;
                self.expansion_bytes = self
                    .expansion_bytes
                    .checked_add(original_size)
                    .filter(|size| *size <= VAULT_IMPORT_EXPANSION_BYTES_MAX)
                    .ok_or_else(|| {
                        vault_import_error("native vault text expansion exceeds limit")
                    })?;
                self.bytes_variable()?;
            }
            _ => return Err(vault_import_error("unknown native vault value tag")),
        }
        Ok(())
    }

    fn validate_metadata_value(&mut self, depth: u32) -> Result<(), StoreError> {
        vault_import_depth(depth)?;
        let tag = self.byte()?;
        match tag {
            0 => {}
            1 => {
                self.take(1)?;
            }
            2 | 3 | 8 => {
                self.take(8)?;
            }
            4 | 5 => self.bytes_fixed()?,
            6 | 7 => {
                let count = self.count_fixed()?;
                for _ in 0..count {
                    if tag == 7 {
                        self.bytes_fixed()?;
                    }
                    self.validate_metadata_value(depth + 1)?;
                }
            }
            9 => {
                self.take(16)?;
            }
            10 => self.validate_metadata_reference()?,
            11 => {
                let count = self.count_fixed()?;
                for _ in 0..count {
                    self.validate_metadata_reference()?;
                }
            }
            _ => return Err(vault_import_error("unknown native vault metadata tag")),
        }
        Ok(())
    }

    fn validate_metadata_reference(&mut self) -> Result<(), StoreError> {
        if self.byte()? > 4 {
            return Err(vault_import_error(
                "unknown native vault metadata reference",
            ));
        }
        self.bytes_fixed()?;
        self.take(8)?;
        Ok(())
    }
}

fn vault_import_depth(depth: u32) -> Result<(), StoreError> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Err(vault_import_error(
            "native vault nesting exceeds depth limit",
        ));
    }
    Ok(())
}

fn vault_import_error(message: &str) -> StoreError {
    StoreError::Serialization(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_import_validation_preserves_v8_v9_and_current_records() {
        let mut row = RowData::new(Vec::new());
        row.named = Some(HashMap::from([
            ("key".into(), Value::text("token")),
            ("value".into(), Value::Secret(vec![1, 2, 3])),
            ("version".into(), Value::Integer(1)),
            ("tombstone".into(), Value::Boolean(false)),
            ("op".into(), Value::text("put")),
            ("_vault_tenant".into(), Value::text("acme")),
        ]));
        let mut entity = UnifiedEntity::new(
            EntityId::new(5),
            EntityKind::TableRow {
                table: "app".into(),
                row_id: 5,
            },
            EntityData::Row(row),
        );
        entity.created_at = 10;
        entity.updated_at = 20;
        entity.sequence_id = 30;
        entity.set_logical_id(EntityId::new(7));
        entity
            .embeddings_mut()
            .push(EmbeddingSlot::new("slot", vec![1.0, 2.0], "model"));
        entity.cross_refs_mut().push(CrossRef::new(
            EntityId::new(5),
            EntityId::new(8),
            "other",
            RefType::RelatedTo,
        ));
        let metadata = Metadata::with_fields(HashMap::from([(
            "tags".into(),
            MetadataValue::Array(vec![MetadataValue::Object(HashMap::from([(
                "scope".into(),
                MetadataValue::String("backup".into()),
            )]))]),
        )]));
        for format_version in [STORE_VERSION_V8, STORE_VERSION_V9, STORE_VERSION_CURRENT] {
            let bytes =
                UnifiedStore::serialize_entity_record(&entity, Some(&metadata), format_version);
            let (decoded, metadata) =
                UnifiedStore::deserialize_vault_entity_record(&bytes, format_version)
                    .expect("valid versioned native record");
            assert_eq!(
                UnifiedStore::serialize_entity_record(&decoded, metadata.as_ref(), format_version),
                bytes,
            );
            let frame = reddb_file::decode_native_entity_record_frame(&bytes)
                .expect("frame")
                .expect("native frame");
            for size in 0..frame.entity.len() {
                let truncated = reddb_file::encode_native_entity_record_frame(
                    &frame.entity[..size],
                    Some(frame.metadata),
                );
                assert!(
                    UnifiedStore::deserialize_vault_entity_record(&truncated, format_version)
                        .is_err(),
                    "format {format_version}, truncated at {size}",
                );
            }
        }
    }
}
