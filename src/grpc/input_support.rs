use super::*;

pub(crate) fn resolve_projection_payload(
    runtime: &GrpcRuntime,
    payload: &JsonValue,
) -> Result<Option<RuntimeGraphProjection>, Status> {
    let named = json_string_field(payload, "projection_name");
    let inline = crate::application::graph_payload::parse_inline_projection(payload);
    runtime
        .graph_use_cases()
        .resolve_projection(named.as_deref(), inline)
        .map_err(to_status)
}
