use super::*;

fn scan_reply(page: ScanPage) -> ScanReply {
    ScanReply {
        collection: page.collection,
        total: page.total as u64,
        next_offset: page.next.map(|cursor| cursor.offset as u64),
        items: page.items.into_iter().map(scan_entity).collect(),
    }
}

fn scan_entity(entity: UnifiedEntity) -> ScanEntity {
    ScanEntity {
        id: entity.id.raw(),
        kind: entity.kind.storage_type().to_string(),
        collection: entity.kind.collection().to_string(),
        json: crate::presentation::entity_json::compact_entity_json_string(&entity),
    }
}

fn query_reply(
    result: RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> QueryReply {
    let records = crate::presentation::query_view::filter_query_records(
        &result.result.records,
        entity_types,
        capabilities,
    );
    QueryReply {
        ok: true,
        mode: format!("{:?}", result.mode).to_lowercase(),
        statement: result.statement.to_string(),
        engine: result.engine.to_string(),
        columns: result.result.columns.clone(),
        record_count: records.len() as u64,
        result_json: unified_result_json_string_with_records(
            &result.result,
            &records,
            entity_types,
            capabilities,
        ),
    }
}

fn unified_result_json_string_with_records(
    result: &crate::storage::query::unified::UnifiedResult,
    records: &[crate::storage::query::unified::UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> String {
    json_to_string(
        &crate::presentation::query_result_json::unified_result_values_only_json_with_records(
            result,
            records,
            crate::presentation::query_view::search_selection_json(entity_types, capabilities),
        ),
    )
    .unwrap_or_else(|_| "{}".to_string())
}

fn grpc_parse_query_filters(
    request: &QueryRequest,
) -> Result<(Option<Vec<String>>, Option<Vec<String>>), Status> {
    crate::application::query_payload::normalize_search_selection(
        &request.entity_types,
        &request.capabilities,
    )
    .map_err(|err| Status::invalid_argument(err.to_string()))
}
