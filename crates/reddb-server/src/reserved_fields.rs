pub(crate) const RESERVED_PUBLIC_ITEM_FIELDS: &[&str] = &[
    "rid",
    "collection",
    "kind",
    "tenant",
    "created_at",
    "updated_at",
];

pub(crate) fn is_reserved_public_item_field(field: &str) -> bool {
    RESERVED_PUBLIC_ITEM_FIELDS
        .iter()
        .any(|reserved| field.eq_ignore_ascii_case(reserved))
}

pub(crate) fn reserved_field_error(field: &str, context: &str) -> crate::RedDBError {
    crate::RedDBError::Query(format!(
        "reserved system field '{field}' cannot be used as a top-level user field in {context}"
    ))
}

pub(crate) fn ensure_no_reserved_public_item_fields<'a, I>(
    fields: I,
    context: &str,
) -> crate::RedDBResult<()>
where
    I: IntoIterator<Item = &'a str>,
{
    for field in fields {
        if is_reserved_public_item_field(field) {
            return Err(reserved_field_error(field, context));
        }
    }
    Ok(())
}

pub(crate) fn validate_physical_metadata_contracts(
    metadata: &crate::physical::PhysicalMetadataFile,
) -> crate::RedDBResult<()> {
    for contract in &metadata.collection_contracts {
        validate_collection_contract(contract)?;
    }
    Ok(())
}

fn validate_collection_contract(
    contract: &crate::physical::CollectionContract,
) -> crate::RedDBResult<()> {
    if contract.declared_model != crate::catalog::CollectionModel::Table {
        return Ok(());
    }

    let context = format!("table '{}'", contract.name);
    for column in &contract.declared_columns {
        if contract.timestamps_enabled
            && (column.name.eq_ignore_ascii_case("created_at")
                || column.name.eq_ignore_ascii_case("updated_at"))
        {
            continue;
        }
        if is_reserved_public_item_field(&column.name) {
            return Err(reserved_field_error(&column.name, &context));
        }
    }

    if let Some(table_def) = &contract.table_def {
        for column in &table_def.columns {
            if contract.timestamps_enabled
                && (column.name.eq_ignore_ascii_case("created_at")
                    || column.name.eq_ignore_ascii_case("updated_at"))
            {
                continue;
            }
            if is_reserved_public_item_field(&column.name) {
                return Err(reserved_field_error(&column.name, &context));
            }
        }
    }

    Ok(())
}
