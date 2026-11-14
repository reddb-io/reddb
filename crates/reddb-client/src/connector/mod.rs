//! Internal connector + REPL used by the `red` and `red_client`
//! binaries (and by `reddb-server`'s rpc_stdio mode).
//!
//! This module is deliberately kept dependency-light. The
//! [`RedDBClient`] gRPC connector itself lives beside the generated
//! stubs in [`reddb-grpc-proto`] so both client and server consumers
//! share the wire reply types directly.
//! The [`http`], [`redwire`], and [`repl`] helpers below are
//! consumed by the `red_client` bin only and stay here because
//! nothing else in the workspace pulls them in.
//!
//! [`reddb-grpc-proto`]: ../../../reddb_grpc_proto/index.html

pub mod http;
pub mod redwire;
pub mod repl;

// Re-export the connector types from their canonical crate so the
// existing `reddb_client::{RedDBClient, repl, …}` import paths
// keep resolving.
pub use reddb_grpc_proto::{
    BulkEntityReply as BulkCreateStatus, EntityReply as CreatedEntity, HealthReply as HealthStatus,
    OperationReply as OperationStatus, QueryReply as QueryResponse, RedDBClient,
};

/// Encode public client bind values for the dependency-light gRPC connector.
pub fn params_to_grpc_values(params: &[crate::Value]) -> Vec<reddb_grpc_proto::QueryValue> {
    use reddb_grpc_proto::query_value::Kind;
    use reddb_grpc_proto::{
        QueryBiometric, QueryBitSignature, QueryFeatureVector, QueryNull, QueryValue, QueryVector,
    };

    params
        .iter()
        .cloned()
        .map(|value| {
            let kind = match value {
                crate::Value::Null => Kind::NullValue(QueryNull {}),
                crate::Value::Bool(value) => Kind::BoolValue(value),
                crate::Value::Int(value) => Kind::IntValue(value),
                crate::Value::Float(value) => Kind::FloatValue(value),
                crate::Value::Text(value) => Kind::TextValue(value),
                crate::Value::Bytes(value) => Kind::BytesValue(value),
                crate::Value::Vector(values) => Kind::VectorValue(QueryVector { values }),
                crate::Value::Json(value) => Kind::JsonValue(value.to_json_string()),
                crate::Value::Timestamp(value) => Kind::TimestampValue(value),
                crate::Value::Uuid(value) => Kind::UuidValue(value.to_vec()),
                crate::Value::Fingerprint(value) => Kind::FingerprintValue(QueryBiometric {
                    format_code: value.format() as u32,
                    data: value.bytes().to_vec(),
                }),
                crate::Value::Face(value) => Kind::FaceValue(QueryBiometric {
                    format_code: value.format() as u32,
                    data: value.bytes().to_vec(),
                }),
                crate::Value::Iris(value) => Kind::IrisValue(QueryBiometric {
                    format_code: value.format() as u32,
                    data: value.bytes().to_vec(),
                }),
                crate::Value::FeatureVector(value) => {
                    Kind::FeatureVectorValue(QueryFeatureVector {
                        space_id: value.space_id().to_vec(),
                        values: value.values().to_vec(),
                    })
                }
                crate::Value::BitSignature(value) => Kind::BitSignatureValue(QueryBitSignature {
                    space_id: value.space_id().to_vec(),
                    bits: value.bits().to_vec(),
                    mask: value.mask().to_vec(),
                }),
            };
            QueryValue { kind: Some(kind) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_grpc_proto::query_value::Kind;

    fn iso_fmr() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FMR\0 20\0");
        bytes.extend_from_slice(&36_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[1, 0, 100, 1]);
        bytes.extend_from_slice(&[0x40, 1, 0, 2, 3, 90]);
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn grpc_params_preserve_biometric_and_feature_metadata() {
        let fingerprint = reddb_types::BiometricSample::with_format(
            reddb_types::BiometricFormat::FingerprintIso19794_2,
            iso_fmr(),
        )
        .expect("valid fingerprint");
        let feature =
            reddb_types::FeatureVector::new([0x11; 16], vec![1.0, -2.0]).expect("valid feature");
        let signature = reddb_types::BitSignature::new([0x22; 16], vec![0xaa], Some(vec![0x0f]))
            .expect("valid signature");

        let encoded = params_to_grpc_values(&[
            crate::Value::Fingerprint(fingerprint),
            crate::Value::FeatureVector(feature),
            crate::Value::BitSignature(signature),
        ]);

        let Some(Kind::FingerprintValue(value)) = &encoded[0].kind else {
            panic!("expected fingerprint")
        };
        assert_eq!(
            value.format_code,
            reddb_types::BiometricFormat::FingerprintIso19794_2 as u32
        );
        let Some(Kind::FeatureVectorValue(value)) = &encoded[1].kind else {
            panic!("expected feature vector")
        };
        assert_eq!(value.space_id, vec![0x11; 16]);
        let Some(Kind::BitSignatureValue(value)) = &encoded[2].kind else {
            panic!("expected bit signature")
        };
        assert_eq!(value.mask, vec![0x0f]);
    }
}
