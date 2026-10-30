//! Offline operator tool for a verified bootstrap-only legacy database.
//!
//! This imports an authenticated full Vault envelope, typed configuration and
//! control-event rows into a NEW directory. It never opens or modifies a legacy
//! pager. The operator must first fence/export the source, verify the complete
//! native catalog and retain an independently restored physical backup.
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use reddb::auth::vault::{Vault, VaultState};
use reddb::json::Value as Json;
use reddb::runtime::query_request::ParamValue;
use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ROWS: usize = 16_384;
const AUDIT_FIELDS: [&str; 14] = [
    "id",
    "ts",
    "kind",
    "outcome",
    "actor_kind",
    "actor_user_id",
    "scope",
    "action",
    "resource",
    "reason",
    "matched_policy_id",
    "request_id",
    "trace_id",
    "fields_json",
];

fn rows<'a>(input: &'a Json, name: &str) -> Result<&'a [Json]> {
    let list = input
        .get(name)
        .and_then(Json::as_array)
        .ok_or("missing row array")?;
    if list.len() > MAX_ROWS {
        return Err("row limit exceeded".into());
    }
    Ok(list)
}

fn validate(input: &Json) -> Result<()> {
    if input.get("format").and_then(Json::as_str) != Some("reddb.operator-bootstrap.v1") {
        return Err("unsupported import format".into());
    }
    if !rows(input, "user_collections")?.is_empty() || !rows(input, "policies")?.is_empty() {
        return Err(
            "user data or policies require a complete migration, not bootstrap import".into(),
        );
    }
    // The source's full physical inventory is required, including collections
    // hidden by SHOW COLLECTIONS. Unknown internal collections are refused too.
    let known_empty = [
        "red.config",
        "red.control_events",
        "red.topology.cluster",
        "red.vault",
        "red_closure",
        "red_commits",
        "red_conflicts",
        "red_materialized_view_defs",
        "red_merge_state",
        "red_migration_deps",
        "red_migrations",
        "red_refs",
        "red_remotes",
        "red_vcs_settings",
        "red_worksets",
    ];
    for entry in rows(input, "native_collections")? {
        let name = entry
            .get("name")
            .and_then(Json::as_str)
            .ok_or("missing catalog name")?;
        let entities = entry
            .get("entities")
            .and_then(Json::as_str)
            .ok_or("missing catalog count")?
            .parse::<u64>()?;
        if name != "red_config" && (!known_empty.contains(&name) || entities != 0) {
            return Err("unhandled native collection or nonempty internal state".into());
        }
    }
    if rows(input, "native_collections")?.is_empty() {
        return Err("complete native catalog required".into());
    }
    if input.get("native_catalog_complete").and_then(Json::as_bool) != Some(true) {
        return Err("incomplete native catalog".into());
    }
    for index in rows(input, "indexes")? {
        if index.get("collection").and_then(Json::as_str) != Some("red_index_registry")
            || index.get("method").and_then(Json::as_str) != Some("btree")
            || index.get("unique").and_then(Json::as_bool).is_none()
        {
            return Err("unsupported source index definition".into());
        }
        identifier(
            index
                .get("name")
                .and_then(Json::as_str)
                .ok_or("missing index name")?,
        )?;
        let columns = index
            .get("columns")
            .and_then(Json::as_array)
            .ok_or("missing index columns")?;
        if columns.is_empty() || columns.len() > 16 {
            return Err("unsupported index columns".into());
        }
        for column in columns {
            identifier(column.as_str().ok_or("invalid index column")?)?;
        }
    }
    for row in rows(input, "config")? {
        let object = row.as_object().ok_or("config row must be an object")?;
        if object.len() != 2
            || row.get("key").and_then(Json::as_str).is_none()
            || row.get("value").is_none()
        {
            return Err("config rows require only key and value".into());
        }
        decode_fields(row)?;
    }
    let mut ids = HashSet::new();
    for row in rows(input, "control_events")? {
        let object = row.as_object().ok_or("audit row must be an object")?;
        if object.len() != AUDIT_FIELDS.len()
            || !AUDIT_FIELDS.iter().all(|field| object.contains_key(*field))
        {
            return Err("unsupported control-event fields".into());
        }
        let id = row
            .get("id")
            .and_then(Json::as_str)
            .ok_or("missing event id")?;
        if !ids.insert(id) {
            return Err("duplicate source control-event id".into());
        }
        decode_fields(row)?;
    }
    Ok(())
}

fn decode_fields(row: &Json) -> Result<HashMap<String, reddb_types::Value>> {
    row.as_object()
        .ok_or("row must be an object")?
        .iter()
        .map(|(key, value)| {
            let value = ParamValue::decode_json(value).map_err(|_| "unsupported typed field")?;
            Ok((key.clone(), value.into()))
        })
        .collect()
}

fn identifier(value: &str) -> Result<()> {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("invalid index identifier".into());
    }
    Ok(())
}

fn import_indexes(runtime: &reddb::RedDBRuntime, input: &Json) -> Result<()> {
    for index in rows(input, "indexes")? {
        runtime
            .db()
            .store()
            .get_or_create_collection("red_index_registry");
        let name = index
            .get("name")
            .and_then(Json::as_str)
            .ok_or("missing index name")?;
        let unique = if index.get("unique").and_then(Json::as_bool) == Some(true) {
            "UNIQUE "
        } else {
            ""
        };
        let columns = index
            .get("columns")
            .and_then(Json::as_array)
            .ok_or("missing index columns")?
            .iter()
            .map(|c| format!("\"{}\"", c.as_str().expect("validated column")))
            .collect::<Vec<_>>()
            .join(",");
        runtime.execute_query(&format!(
            "CREATE {unique}INDEX \"{name}\" ON red_index_registry ({columns}) USING BTREE"
        ))?;
    }
    Ok(())
}

fn canonical_vault(state: &VaultState) -> Result<Vec<String>> {
    let text = String::from_utf8(state.serialize())?;
    let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
    lines.sort();
    Ok(lines)
}

fn insert_rows(runtime: &reddb::RedDBRuntime, collection: &str, rows: &[Json]) -> Result<()> {
    let store = runtime.db().store();
    store.get_or_create_collection(collection);
    for row in rows {
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(collection),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(decode_fields(row)?),
                schema: None,
            }),
        );
        store.insert_auto(collection, entity)?;
    }
    Ok(())
}

fn import(input: &Json, destination: &Path, certificate: &str) -> Result<()> {
    validate(input)?;
    let blob = input
        .get("vault_blob")
        .and_then(Json::as_str)
        .ok_or("missing sealed full Vault")?;
    let state = Vault::unseal_logical_export_with_certificate(blob, certificate)?;
    if !state.bootstrapped || state.master_secret.is_none() || state.users.is_empty() {
        return Err("complete bootstrapped Vault required".into());
    }
    // create_dir is exclusive. A failure retains the partial candidate for
    // inspection; an existing directory, source file or acknowledged write is
    // never overwritten or silently resumed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(destination)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(destination)?;
    let path = destination.join("data.rdb");
    let runtime = reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(&path))?;
    insert_rows(&runtime, "red_config", rows(input, "config")?)?;
    // SQL correctly keeps this collection read-only. Only this offline tool,
    // with a new destination and a validated source, uses the storage API.
    insert_rows(
        &runtime,
        "red.control_events",
        rows(input, "control_events")?,
    )?;
    let pager = Arc::clone(
        runtime
            .db()
            .store()
            .pager()
            .ok_or("persistent pager required")?,
    );
    let vault = Vault::with_certificate(&pager, certificate)?;
    vault.save(&pager, &state)?;
    runtime.checkpoint()?;
    let restored = vault.load(&pager)?.ok_or("restored Vault missing")?;
    if canonical_vault(&state)? != canonical_vault(&restored)? {
        return Err("complete Vault readback differs".into());
    }
    println!("{{\"ok\":true,\"config_rows\":{},\"control_events\":{},\"users\":{},\"api_keys\":{},\"vault_keys\":{}}}",
        rows(input,"config")?.len(), rows(input,"control_events")?.len(), state.users.len(), state.api_keys.len(), state.kv.len());
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("Usage: operator_import_bootstrap PRIVATE_EXPORT.json NEW_DIRECTORY".into());
    }
    let source = Path::new(&args[1]);
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("private export must be a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("private export must have mode 0600".into());
        }
    }
    let input: Json = reddb::json::from_str(&fs::read_to_string(source)?)?;
    let certificate = reddb::utils::env_with_file_fallback("REDDB_CERTIFICATE")
        .ok_or("REDDB_CERTIFICATE[_FILE] required")?;
    import(&input, Path::new(&args[2]), &certificate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Json {
        reddb::json!({"format":"reddb.operator-bootstrap.v1","user_collections":[],"policies":[],
            "native_catalog_complete":true,"native_collections":[{"name":"red_config","entities":"1"}],
            "indexes":[],"config":[{"key":"fixture.integer","value":1699999999999999999i64}],"control_events":[]})
    }

    #[test]
    fn preserves_large_integer_values_without_float_conversion() {
        let input = input();
        validate(&input).expect("valid fixture");
        let fields =
            decode_fields(&rows(&input, "config").expect("config")[0]).expect("typed fields");
        assert_eq!(
            fields.get("value"),
            Some(&reddb_types::Value::Integer(1699999999999999999))
        );
    }

    #[test]
    fn imports_complete_vault_typed_config_audit_and_empty_registry_index_then_reopens(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(
            directory.path().join("source.rdb"),
        ))?;
        let pager = Arc::clone(source.db().store().pager().ok_or("source pager")?);
        let auth = reddb::auth::AuthStore::with_vault(
            reddb::auth::AuthConfig::default(),
            Arc::clone(&pager),
        )?;
        let bootstrap = auth.bootstrap("fixture_admin", "fixture-password-is-private")?;
        let certificate = bootstrap.certificate.ok_or("certificate")?;
        let vault = Vault::with_certificate(&pager, &certificate)?;
        let mut state = vault.load(&pager)?.ok_or("source Vault")?;
        state
            .kv
            .insert("red.secret.fixture".into(), "private fixture value".into());
        let mut input = input();
        input["vault_blob"] = Json::String(vault.seal_logical_export(&state)?);
        input["control_events"] = reddb::json!([{
            "id":"retained-event-id", "ts":1699999999999999999i64, "kind":"config.write", "outcome":"ok",
            "actor_kind":"system", "actor_user_id":null, "scope":null, "action":"fixture",
            "resource":null,"reason":null,"matched_policy_id":null,"request_id":null,"trace_id":null,"fields_json":"{}"
        }]);
        input["indexes"] = reddb::json!([{"collection":"red_index_registry","name":"retained_registry_index",
            "columns":["value"],"unique":true,"method":"btree"}]);
        let target = directory.path().join("new");
        import(&input, &target, &certificate)?;
        let before = fs::read(target.join("data.rdb"))?;
        assert!(
            import(&input, &target, &certificate).is_err(),
            "existing destination must be refused"
        );
        assert_eq!(fs::read(target.join("data.rdb"))?, before);
        let reopened = reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(
            target.join("data.rdb"),
        ))?;
        let pager = Arc::clone(reopened.db().store().pager().ok_or("target pager")?);
        let restored = Vault::with_certificate(&pager, &certificate)?
            .load(pager)?
            .ok_or("restored Vault")?;
        assert_eq!(canonical_vault(&state)?, canonical_vault(&restored)?);
        assert_eq!(
            reopened.db().store().get_config("fixture.integer"),
            Some(reddb_types::Value::Integer(1699999999999999999))
        );
        let events = reopened
            .db()
            .store()
            .get_collection("red.control_events")
            .ok_or("audit collection")?
            .query_all(|_| true);
        assert!(events.iter().any(
            |event| event.data.as_row().and_then(|row| row.get_field("ts"))
                == Some(&reddb_types::Value::Integer(1699999999999999999))
        ));
        let restored_auth = reddb::auth::AuthStore::with_vault(
            reddb::auth::AuthConfig::default(),
            Arc::clone(&pager),
        )?;
        restored_auth.authenticate("fixture_admin", "fixture-password-is-private")?;
        Ok(())
    }

    #[test]
    fn refuses_hidden_nonempty_native_state_before_creating_destination() {
        let mut input = input();
        input["native_collections"] = reddb::json!([{"name":"red_commits","entities":"1"}]);
        assert!(validate(&input).is_err());
        input["native_collections"] = reddb::json!([{"name":"unknown","entities":"0"}]);
        assert!(validate(&input).is_err());
    }
}
