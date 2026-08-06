use super::*;

pub(crate) use crate::presentation::query_result::{
    ask_answer_tokens_from_unified_result, ask_result_from_unified_result,
};

pub(crate) fn scan_reply(page: ScanPage) -> ScanReply {
    ScanReply {
        collection: page.collection,
        total: page.total as u64,
        next_offset: page.next.map(|cursor| cursor.offset as u64),
        items: page.items.into_iter().map(scan_entity).collect(),
    }
}

pub(crate) fn scan_entity(entity: UnifiedEntity) -> ScanEntity {
    ScanEntity {
        id: entity.id.raw(),
        kind: entity.kind.storage_type().to_string(),
        collection: entity.kind.collection().to_string(),
        json: crate::presentation::entity_json::compact_entity_json_string(&entity),
    }
}

pub(crate) fn query_reply(
    result: RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> QueryReply {
    crate::presentation::query_result::proto_reply(&result, entity_types, capabilities)
}

pub(crate) fn grpc_parse_query_filters(
    request: &QueryRequest,
) -> Result<(Option<Vec<String>>, Option<Vec<String>>), Status> {
    crate::application::query_payload::normalize_search_selection(
        &request.entity_types,
        &request.capabilities,
    )
    .map_err(|err| Status::invalid_argument(err.to_string()))
}
