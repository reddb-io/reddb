//! Measurement-only JS/WASM bridge. Not part of the published JS client.
#[cfg(target_arch = "wasm32")]
use reddb_wire::{
    query_with_params::{encode_query_with_params, ParamValue},
    redwire::{build_query_with_params_frame, encode_frame},
};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn query_frame(sql: &str, value: ParamValue) -> Result<Vec<u8>, JsValue> {
    let payload = encode_query_with_params(sql, &[value])
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let frame = build_query_with_params_frame(1, payload)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(encode_frame(&frame))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = encodeVectorQueryFrame)]
pub fn encode_vector_query_frame(sql: &str, value: Vec<f32>) -> Result<Vec<u8>, JsValue> {
    query_frame(sql, ParamValue::Vector(value))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = encodeBytesQueryFrame)]
pub fn encode_bytes_query_frame(sql: &str, value: Vec<u8>) -> Result<Vec<u8>, JsValue> {
    query_frame(sql, ParamValue::Bytes(value))
}
