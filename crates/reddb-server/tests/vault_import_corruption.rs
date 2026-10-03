use std::sync::Arc;

use reddb_server::auth::{AuthConfig, AuthStore};
use reddb_server::storage::StorageDeployPreset;
use reddb_server::{RedDBOptions, RedDBRuntime};

const CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn runtime(directory: &std::path::Path, name: &str) -> (RedDBRuntime, Arc<AuthStore>) {
    let options = RedDBOptions::persistent(directory.join(name))
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("persistent storage profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, CERTIFICATE)
            .expect("vault"),
    );
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(Arc::clone(&auth));
    (runtime, auth)
}

fn framed(entity: &[u8], metadata: Option<&[u8]>) -> String {
    hex::encode(reddb_file::encode_native_entity_record_frame(
        entity, metadata,
    ))
}

fn reject(runtime: &RedDBRuntime, record: String, collection: &str) {
    assert!(
        runtime
            .import_vault_collection_records(
                collection,
                &[record],
                runtime.db().store().format_version(),
            )
            .is_err(),
        "malformed record must return an error for {collection}",
    );
    assert!(runtime.db().store().get_collection(collection).is_none());
}

#[test]
fn native_vault_import_rejects_truncated_frames_and_every_entity_prefix() {
    let directory = tempfile::tempdir().expect("directory");
    let (source, source_auth) = runtime(directory.path(), "source.rdb");
    source.execute_query("CREATE VAULT app").expect("vault");
    source
        .execute_query("VAULT PUT app.token = 'backup-value' TAGS ['backup-tag']")
        .expect("entry");
    let records = source.export_vault_collection_records("app").expect("dump");
    let bytes = hex::decode(&records[0]).expect("record bytes");
    let frame = reddb_file::decode_native_entity_record_frame(&bytes)
        .expect("frame")
        .expect("native frame");
    let (destination, destination_auth) = runtime(directory.path(), "destination.rdb");
    destination_auth
        .vault_kv_try_import(source_auth.vault_kv_snapshot())
        .expect("keys");

    for size in 0..bytes.len() {
        reject(&destination, hex::encode(&bytes[..size]), "truncated");
    }
    for size in 0..frame.entity.len() {
        reject(
            &destination,
            framed(&frame.entity[..size], None),
            "entity_prefix",
        );
    }
    for size in 1..frame.metadata.len() {
        reject(
            &destination,
            framed(frame.entity, Some(&frame.metadata[..size])),
            "metadata_prefix",
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    reject(&destination, hex::encode(trailing), "trailing");
    assert_eq!(
        destination
            .import_vault_collection_records("app", &records, source.db().store().format_version())
            .expect("valid native import"),
        1,
    );
    let revealed = destination
        .execute_query("VAULT REVEAL app.token")
        .expect("reveal");
    assert_eq!(
        revealed.result.records[0].get("value"),
        Some(&reddb_types::Value::text("backup-value")),
    );
    let history = destination
        .execute_query("VAULT HISTORY app.token")
        .expect("history");
    assert!(format!("{:?}", history.result.records[0].get("tags")).contains("backup-tag"));
}

#[test]
fn native_vault_import_bounds_varints_lengths_counts_and_metadata_depth() {
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = runtime(directory.path(), "corruption.rdb");
    reject(&runtime, framed(&[0x80, 0x00], None), "id_without_kind");
    reject(&runtime, framed(&[0xff; 10], None), "id_overflow");
    let huge = [0xff, 0xff, 0xff, 0xff, 0x0f];
    let mut table_length = vec![0, 0];
    table_length.extend_from_slice(&huge);
    reject(&runtime, framed(&table_length, None), "table_length");
    // id, TableRow kind, one-byte table, row id, named RowData.
    let header = [0, 0, 1, b'a', 0, 6];
    let mut field_count = header.to_vec();
    field_count.extend_from_slice(&huge);
    reject(&runtime, framed(&field_count, None), "field_count");
    for tag in [5, 11, 28, 48] {
        let mut value = header.to_vec();
        value.extend_from_slice(&[1, 1, b'k', tag]);
        value.extend_from_slice(&huge);
        reject(&runtime, framed(&value, None), &format!("value_{tag}"));
    }
    let mut compressed = header.to_vec();
    compressed.extend_from_slice(&[1, 1, b'k', 0x85]);
    compressed.extend_from_slice(&huge);
    compressed.push(0);
    reject(&runtime, framed(&compressed, None), "compressed_expansion");
    let mut nested_value = header.to_vec();
    nested_value.extend_from_slice(&[1, 1, b'k']);
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH + 2 {
        nested_value.extend_from_slice(&[28, 1]);
    }
    nested_value.push(0);
    reject(&runtime, framed(&nested_value, None), "value_depth");

    runtime.execute_query("CREATE VAULT app").expect("vault");
    runtime
        .execute_query("VAULT PUT app.token = 'value'")
        .expect("entry");
    let record = runtime
        .export_vault_collection_records("app")
        .expect("dump");
    let bytes = hex::decode(&record[0]).expect("bytes");
    let entity = reddb_file::decode_native_entity_record_frame(&bytes)
        .expect("frame")
        .expect("native frame")
        .entity;
    for tag in [4, 6, 7, 11] {
        // One metadata field named k, then a forged string/container count.
        let mut metadata = vec![1, 0, 0, 0, 1, 0, 0, 0, b'k', tag];
        metadata.extend_from_slice(&u32::MAX.to_le_bytes());
        reject(
            &runtime,
            framed(entity, Some(&metadata)),
            &format!("metadata_{tag}"),
        );
    }
    let mut metadata = vec![1, 0, 0, 0, 1, 0, 0, 0, b'k'];
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH + 2 {
        metadata.extend_from_slice(&[6, 1, 0, 0, 0]);
    }
    metadata.push(0);
    reject(&runtime, framed(entity, Some(&metadata)), "metadata_depth");
}
