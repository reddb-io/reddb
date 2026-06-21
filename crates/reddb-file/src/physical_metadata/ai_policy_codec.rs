//! AI-policy JSON codecs for the physical metadata document contract.
//!
//! Split out of the parent `physical_metadata` module to keep that file under
//! the 2000-line file-contract budget enforced by
//! `tests/layout_authority/storage.rs`. Behaviour is unchanged — these are the
//! same `*_json_value` / `*_from_json_value` helpers, moved verbatim. Only the
//! two entry points the parent calls (`ai_policy_json_value` /
//! `ai_policy_from_json_value`) are re-exposed to it via `pub(super)`.

use super::json_helpers::*;
use super::types::*;
use crate::RdbFileResult;

pub(super) fn ai_policy_json_value(policy: &PhysicalAiPolicy) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "embed".to_string(),
        policy
            .embed
            .as_ref()
            .map(ai_embed_policy_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "moderate".to_string(),
        policy
            .moderate
            .as_ref()
            .map(ai_moderate_policy_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "vision".to_string(),
        policy
            .vision
            .as_ref()
            .map(ai_vision_policy_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    serde_json::Value::Object(object)
}

fn ai_embed_policy_json_value(policy: &PhysicalAiEmbedPolicy) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("fields".to_string(), string_array_json(&policy.fields));
    object.insert(
        "provider".to_string(),
        serde_json::Value::String(policy.provider.clone()),
    );
    object.insert(
        "model".to_string(),
        serde_json::Value::String(policy.model.clone()),
    );
    serde_json::Value::Object(object)
}

fn ai_moderate_policy_json_value(policy: &PhysicalAiModeratePolicy) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("fields".to_string(), string_array_json(&policy.fields));
    object.insert(
        "provider".to_string(),
        serde_json::Value::String(policy.provider.clone()),
    );
    object.insert(
        "model".to_string(),
        serde_json::Value::String(policy.model.clone()),
    );
    object.insert(
        "sync_gate".to_string(),
        serde_json::Value::Bool(policy.sync_gate),
    );
    object.insert(
        "degraded_mode".to_string(),
        serde_json::Value::String(policy.degraded_mode.clone()),
    );
    object.insert(
        "reject_action".to_string(),
        serde_json::Value::String(policy.reject_action.clone()),
    );
    object.insert(
        "hard_delete_on_reject".to_string(),
        serde_json::Value::Bool(policy.hard_delete_on_reject),
    );
    serde_json::Value::Object(object)
}

fn ai_vision_policy_json_value(policy: &PhysicalAiVisionPolicy) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "image_field".to_string(),
        serde_json::Value::String(policy.image_field.clone()),
    );
    object.insert(
        "output_kinds".to_string(),
        string_array_json(&policy.output_kinds),
    );
    object.insert(
        "provider".to_string(),
        serde_json::Value::String(policy.provider.clone()),
    );
    object.insert(
        "model".to_string(),
        serde_json::Value::String(policy.model.clone()),
    );
    serde_json::Value::Object(object)
}

pub(super) fn ai_policy_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAiPolicy> {
    let object = expect_object(value, "physical ai policy")?;
    Ok(PhysicalAiPolicy {
        embed: match object.get("embed") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(ai_embed_policy_from_json_value(value)?),
        },
        moderate: match object.get("moderate") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(ai_moderate_policy_from_json_value(value)?),
        },
        vision: match object.get("vision") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(ai_vision_policy_from_json_value(value)?),
        },
    })
}

fn ai_embed_policy_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAiEmbedPolicy> {
    let object = expect_object(value, "physical ai embed policy")?;
    Ok(PhysicalAiEmbedPolicy {
        fields: string_array_from_json(object.get("fields")).unwrap_or_default(),
        provider: json_string_required(object, "provider")?,
        model: json_string_required(object, "model")?,
    })
}

fn ai_moderate_policy_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAiModeratePolicy> {
    let object = expect_object(value, "physical ai moderate policy")?;
    Ok(PhysicalAiModeratePolicy {
        fields: string_array_from_json(object.get("fields")).unwrap_or_default(),
        provider: json_string_required(object, "provider")?,
        model: json_string_required(object, "model")?,
        sync_gate: object
            .get("sync_gate")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        degraded_mode: object
            .get("degraded_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("open")
            .to_string(),
        reject_action: object
            .get("reject_action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("reject")
            .to_string(),
        hard_delete_on_reject: object
            .get("hard_delete_on_reject")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn ai_vision_policy_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAiVisionPolicy> {
    let object = expect_object(value, "physical ai vision policy")?;
    Ok(PhysicalAiVisionPolicy {
        image_field: json_string_required(object, "image_field")?,
        output_kinds: string_array_from_json(object.get("output_kinds")).unwrap_or_default(),
        provider: json_string_required(object, "provider")?,
        model: json_string_required(object, "model")?,
    })
}
