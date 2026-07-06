//! First-boot bootstrap manifest support.
//!
//! The manifest is applied only before `system.bootstrap.completed` is
//! persisted by the server bootstrap path. It is intentionally a thin
//! translation layer over existing public surfaces: `AuthStore` for users
//! and IAM policies, `ConfigRegistry` for managed guardrails, and
//! `red_config` rows for config values.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::auth::policies::{evaluate, Decision, EvalContext, Policy, ResourceRef};
use crate::auth::registry::{
    ConfigRegistry, ConfigRegistryDraft, ConfigRegistryEntry, EvidenceRequirement, Mutability,
    Sensitivity, ACTION_REGISTER, RESOURCE_KIND,
};
use crate::auth::store::{AuthStore, PrincipalRef};
use crate::auth::{Role, User, UserId};
use crate::runtime::RedDBRuntime;
use crate::serde_json::{Map as JsonMap, Value as JsonValue};
use crate::service_cli::BootstrapConfig;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

pub const MANIFEST_ENV: &str = "REDDB_BOOTSTRAP_MANIFEST";
const REGISTRY_STATE_KEY: &str = "system.bootstrap.manifest.registry_entries";

pub fn apply_manifest_file(
    runtime: &RedDBRuntime,
    auth_store: &Arc<AuthStore>,
    registry: &Arc<ConfigRegistry>,
    path: &Path,
) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read {MANIFEST_ENV} {}: {err}", path.display()))?;
    let manifest = BootstrapManifest::parse(&raw)?;
    manifest.apply(runtime, auth_store, registry)
}

pub(crate) fn bootstrap_config_from_manifest_file(
    path: &Path,
) -> Result<Option<BootstrapConfig>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read {MANIFEST_ENV} {}: {err}", path.display()))?;
    bootstrap_config_from_manifest_json(&raw)
}

pub fn rehydrate_manifest_registry(
    runtime: &RedDBRuntime,
    registry: &Arc<ConfigRegistry>,
) -> Result<(), String> {
    let Some(value) = runtime.db().store().get_config(REGISTRY_STATE_KEY) else {
        return Ok(());
    };
    let json = match value {
        Value::Json(bytes) => crate::serde_json::from_slice::<JsonValue>(&bytes)
            .map_err(|err| format!("parse persisted bootstrap registry: {err}"))?,
        Value::Text(s) => crate::serde_json::from_str::<JsonValue>(s.as_ref())
            .map_err(|err| format!("parse persisted bootstrap registry: {err}"))?,
        other => {
            return Err(format!(
                "persisted bootstrap registry must be JSON, got {other:?}"
            ));
        }
    };
    let entries = json
        .as_array()
        .ok_or_else(|| "persisted bootstrap registry must be an array".to_string())?;
    for (idx, value) in entries.iter().enumerate() {
        let entry = registry_entry_from_json(value, idx)?;
        registry
            .restore_bootstrap_entry(entry)
            .map_err(|err| format!("restore bootstrap registry entry {idx}: {err}"))?;
    }
    Ok(())
}

fn bootstrap_config_from_manifest_json(raw: &str) -> Result<Option<BootstrapConfig>, String> {
    let root: JsonValue =
        crate::serde_json::from_str(raw).map_err(|err| format!("parse manifest JSON: {err}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "bootstrap manifest must be a JSON object".to_string())?;
    let intent = obj
        .get("bootstrap")
        .and_then(|value| value.as_object())
        .unwrap_or(obj);

    let has_intent = intent.contains_key("preset")
        || intent.contains_key("bootstrap_preset")
        || intent.contains_key("admin")
        || intent.contains_key("bootstrap_admin")
        || intent.contains_key("cloud_head_admin")
        || intent.contains_key("customer_admin");
    if !has_intent {
        return Ok(None);
    }

    let admin = parse_bootstrap_principal(intent, "admin", "bootstrap_admin")?;
    let cloud_head = parse_bootstrap_principal(intent, "cloud_head_admin", "cloud_head_admin")?;
    let customer = parse_bootstrap_principal(intent, "customer_admin", "customer_admin")?;

    Ok(Some(BootstrapConfig {
        preset: optional_string(intent, "preset")
            .or_else(|| optional_string(intent, "bootstrap_preset")),
        manifest: None,
        admin_username: admin.as_ref().map(|principal| principal.username.clone()),
        admin_password: admin.map(|principal| principal.password),
        cloud_head_admin: cloud_head
            .as_ref()
            .map(|principal| principal.username.clone()),
        cloud_head_admin_password: cloud_head.map(|principal| principal.password),
        customer_admin: customer
            .as_ref()
            .map(|principal| principal.username.clone()),
        customer_admin_password: customer.map(|principal| principal.password),
        ..BootstrapConfig::default()
    }))
}

struct BootstrapPrincipal {
    username: String,
    password: String,
}

fn parse_bootstrap_principal(
    obj: &JsonMap<String, JsonValue>,
    field: &str,
    alias: &str,
) -> Result<Option<BootstrapPrincipal>, String> {
    let Some(value) = obj.get(field).or_else(|| obj.get(alias)) else {
        return Ok(None);
    };
    let principal = value
        .as_object()
        .ok_or_else(|| format!("bootstrap manifest `{field}` must be an object"))?;
    if principal.contains_key("password") || principal.contains_key("secret") {
        return Err(format!(
            "bootstrap manifest `{field}` must use password_file, not inline plaintext"
        ));
    }
    let username = optional_string(principal, "username")
        .or_else(|| optional_string(principal, "name"))
        .ok_or_else(|| format!("bootstrap manifest `{field}` requires username"))?;
    let password_file = optional_string(principal, "password_file")
        .or_else(|| optional_string(principal, "password_path"))
        .ok_or_else(|| format!("bootstrap manifest `{field}` requires password_file"))?;
    let password = read_secret_file(&password_file, field)?;
    Ok(Some(BootstrapPrincipal { username, password }))
}

fn read_secret_file(path: &str, field: &str) -> Result<String, String> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return Err(format!(
            "bootstrap manifest `{field}` password_file is empty"
        ));
    }
    let value = std::fs::read_to_string(trimmed_path)
        .map_err(|err| format!("read bootstrap manifest `{field}` password_file: {err}"))?
        .trim_end_matches(['\n', '\r'])
        .to_string();
    if value.is_empty() {
        return Err(format!(
            "bootstrap manifest `{field}` password_file produced an empty secret"
        ));
    }
    Ok(value)
}

struct BootstrapManifest {
    users: Vec<ManifestUser>,
    policies: Vec<Policy>,
    managed_policies: Vec<ManagedPolicy>,
    attachments: Vec<PolicyAttachment>,
    registry_entries: Vec<ManifestRegistryEntry>,
    managed_config_namespaces: Vec<ManifestRegistryEntry>,
    config: Vec<ManifestConfig>,
    actor: String,
}

struct ManifestUser {
    username: String,
    password: String,
    role: Role,
    tenant: Option<String>,
}

struct ManagedPolicy {
    policy: Policy,
    required_resource: String,
    evidence: EvidenceRequirement,
}

struct PolicyAttachment {
    user: Option<String>,
    group: Option<String>,
    policy: String,
}

struct ManifestRegistryEntry {
    id: String,
    resource_type: String,
    schema: String,
    mutability: Mutability,
    sensitivity: Sensitivity,
    managed: bool,
    required_action: String,
    required_resource: String,
    evidence: EvidenceRequirement,
}

struct ManifestConfig {
    key: String,
    value: Value,
}

impl BootstrapManifest {
    fn parse(raw: &str) -> Result<Self, String> {
        let root: JsonValue = crate::serde_json::from_str(raw)
            .map_err(|err| format!("parse manifest JSON: {err}"))?;
        let obj = root
            .as_object()
            .ok_or_else(|| "bootstrap manifest must be a JSON object".to_string())?;

        let users = parse_users(array_field(obj, "users")?)?;
        let policies = parse_policies(array_field(obj, "policies")?, "policies")?;
        let managed_policies = parse_managed_policies(array_field(obj, "managed_policies")?)?;
        let attachments = parse_attachments(array_field(obj, "attachments")?)?;
        let mut registry_values = Vec::new();
        registry_values.extend_from_slice(array_field(obj, "registry_entries")?);
        registry_values.extend_from_slice(array_field(obj, "registry")?);
        let registry_entries = parse_registry_entries(&registry_values)?;
        let managed_config_namespaces =
            parse_managed_config_namespaces(array_field(obj, "managed_config_namespaces")?)?;
        let config = parse_config(array_field(obj, "config")?)?;
        let actor = obj
            .get("actor")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| users.first().map(|u| u.username.clone()))
            .ok_or_else(|| "bootstrap manifest requires actor or at least one user".to_string())?;

        let mut user_ids = HashSet::new();
        for user in &users {
            if !user_ids.insert(user_id(&user.tenant, &user.username)) {
                return Err(format!("duplicate manifest user `{}`", user.username));
            }
        }

        let mut policy_ids = HashSet::new();
        for policy in policies
            .iter()
            .chain(managed_policies.iter().map(|p| &p.policy))
        {
            if !policy_ids.insert(policy.id.clone()) {
                return Err(format!("duplicate manifest policy `{}`", policy.id));
            }
        }

        for attachment in &attachments {
            if !policy_ids.contains(&attachment.policy) {
                return Err(format!(
                    "policy attachment references unknown manifest policy `{}`",
                    attachment.policy
                ));
            }
            if let Some(user) = attachment.user.as_ref() {
                if !user_ids.contains(&UserId::platform(user)) {
                    return Err(format!(
                        "policy attachment references unknown manifest user `{user}`"
                    ));
                }
            }
            if attachment.user.is_none() && attachment.group.is_none() {
                return Err("policy attachment requires user or group".to_string());
            }
        }

        validate_registry_authorization_plan(
            &users,
            &policies,
            &managed_policies,
            &attachments,
            &registry_entries,
            &managed_config_namespaces,
            &actor,
        )?;

        Ok(Self {
            users,
            policies,
            managed_policies,
            attachments,
            registry_entries,
            managed_config_namespaces,
            config,
            actor,
        })
    }

    fn apply(
        &self,
        runtime: &RedDBRuntime,
        auth_store: &Arc<AuthStore>,
        registry: &Arc<ConfigRegistry>,
    ) -> Result<String, String> {
        self.validate_against_current_state(auth_store, registry)?;

        for user in &self.users {
            auth_store
                .create_user_in_tenant(
                    user.tenant.as_deref(),
                    &user.username,
                    &user.password,
                    user.role,
                )
                .map_err(|err| format!("create user `{}`: {err}", user.username))?;
        }

        for policy in &self.policies {
            auth_store
                .put_policy(policy.clone())
                .map_err(|err| format!("install policy `{}`: {err}", policy.id))?;
        }
        for managed in &self.managed_policies {
            auth_store
                .put_policy(managed.policy.clone())
                .map_err(|err| format!("install managed policy `{}`: {err}", managed.policy.id))?;
        }
        for attachment in &self.attachments {
            let principal = match (&attachment.user, &attachment.group) {
                (Some(user), None) => PrincipalRef::User(UserId::platform(user)),
                (None, Some(group)) => PrincipalRef::Group(group.clone()),
                (Some(_), Some(_)) => {
                    return Err("policy attachment cannot specify both user and group".to_string());
                }
                (None, None) => return Err("policy attachment requires user or group".to_string()),
            };
            auth_store
                .attach_policy(principal, &attachment.policy)
                .map_err(|err| format!("attach policy `{}`: {err}", attachment.policy))?;
        }

        let (actor, actor_user) = self.actor(auth_store)?;
        let ctx = registry_context(&actor_user);
        let now_ms = current_unix_ms();

        let mut registered = Vec::new();
        for entry in &self.registry_entries {
            registered.push(register_entry(
                registry, auth_store, &actor, &ctx, entry, now_ms,
            )?);
        }
        for managed in &self.managed_policies {
            let entry = ManifestRegistryEntry {
                id: managed.policy.id.clone(),
                resource_type: crate::auth::managed_policy::RESOURCE_TYPE_POLICY.to_string(),
                schema: "iam_policy".to_string(),
                mutability: Mutability::Immutable,
                sensitivity: Sensitivity::Internal,
                managed: true,
                required_action: "policy:*".to_string(),
                required_resource: managed.required_resource.clone(),
                evidence: managed.evidence,
            };
            registered.push(register_entry(
                registry, auth_store, &actor, &ctx, &entry, now_ms,
            )?);
        }
        for entry in &self.managed_config_namespaces {
            registered.push(register_entry(
                registry, auth_store, &actor, &ctx, entry, now_ms,
            )?);
        }

        for config in &self.config {
            insert_config_value_if_absent(runtime, &config.key, config.value.clone())?;
        }
        if !registered.is_empty() {
            persist_registry_state(runtime, &registered)?;
        }

        Ok(actor.to_string())
    }

    fn validate_against_current_state(
        &self,
        auth_store: &AuthStore,
        registry: &ConfigRegistry,
    ) -> Result<(), String> {
        for user in &self.users {
            if auth_store
                .get_user(user.tenant.as_deref(), &user.username)
                .is_some()
            {
                return Err(format!("manifest user `{}` already exists", user.username));
            }
        }
        for policy in self
            .policies
            .iter()
            .chain(self.managed_policies.iter().map(|p| &p.policy))
        {
            if auth_store.get_policy(&policy.id).is_some() {
                return Err(format!("manifest policy `{}` already exists", policy.id));
            }
        }
        for entry in &self.registry_entries {
            if registry.get_active(&entry.id).is_some() {
                return Err(format!("registry entry `{}` already exists", entry.id));
            }
        }
        for entry in &self.managed_config_namespaces {
            if registry.get_active(&entry.id).is_some() {
                return Err(format!("registry entry `{}` already exists", entry.id));
            }
        }
        for managed in &self.managed_policies {
            if registry.get_active(&managed.policy.id).is_some() {
                return Err(format!(
                    "registry entry `{}` already exists",
                    managed.policy.id
                ));
            }
        }
        Ok(())
    }

    fn actor(&self, auth_store: &AuthStore) -> Result<(UserId, User), String> {
        let actor = UserId::platform(&self.actor);
        let user = auth_store
            .get_user(None, &self.actor)
            .ok_or_else(|| format!("manifest actor `{}` does not exist", self.actor))?;
        Ok((actor, user))
    }
}

fn parse_users(values: &[JsonValue]) -> Result<Vec<ManifestUser>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let obj = object_at(value, "users", idx)?;
            let username = required_string(obj, "username", "users", idx)?;
            if obj.contains_key("password") && obj.contains_key("password_file") {
                return Err(format!(
                    "users[{idx}] must specify only one of password or password_file"
                ));
            }
            let password = if let Some(path) = optional_string(obj, "password_file") {
                read_secret_file(&path, "users.password_file")?
            } else {
                required_string(obj, "password", "users", idx)?
            };
            if password.is_empty() {
                return Err(format!("users[{idx}].password is required"));
            }
            let role = Role::from_str(&required_string(obj, "role", "users", idx)?)
                .ok_or_else(|| format!("users[{idx}].role must be read, write, or admin"))?;
            if obj.contains_key("system_owned") {
                return Err(format!(
                    "users[{idx}].system_owned is no longer supported; use explicit policies"
                ));
            }
            Ok(ManifestUser {
                username,
                password,
                role,
                tenant: optional_string(obj, "tenant"),
            })
        })
        .collect()
}

fn parse_policies(values: &[JsonValue], field: &str) -> Result<Vec<Policy>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let document = value
                .as_object()
                .and_then(|obj| obj.get("document"))
                .unwrap_or(value);
            Policy::from_json_str(&document.to_string_compact())
                .map_err(|err| format!("{field}[{idx}] is not a valid policy: {err}"))
        })
        .collect()
}

fn parse_managed_policies(values: &[JsonValue]) -> Result<Vec<ManagedPolicy>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let obj = object_at(value, "managed_policies", idx)?;
            let document = obj.get("document").unwrap_or(value);
            let policy = Policy::from_json_str(&document.to_string_compact())
                .map_err(|err| format!("managed_policies[{idx}] is not a valid policy: {err}"))?;
            Ok(ManagedPolicy {
                required_resource: optional_string(obj, "required_resource")
                    .unwrap_or_else(|| format!("policy:{}", policy.id)),
                evidence: obj
                    .get("evidence")
                    .and_then(|v| v.as_str())
                    .map(parse_evidence)
                    .transpose()?
                    .unwrap_or(EvidenceRequirement::Metadata),
                policy,
            })
        })
        .collect()
}

fn parse_attachments(values: &[JsonValue]) -> Result<Vec<PolicyAttachment>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let obj = object_at(value, "attachments", idx)?;
            Ok(PolicyAttachment {
                user: optional_string(obj, "user"),
                group: optional_string(obj, "group"),
                policy: required_string(obj, "policy", "attachments", idx)?,
            })
        })
        .collect()
}

fn parse_registry_entries(values: &[JsonValue]) -> Result<Vec<ManifestRegistryEntry>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| parse_registry_entry(value, "registry_entries", idx, None))
        .collect()
}

fn parse_managed_config_namespaces(
    values: &[JsonValue],
) -> Result<Vec<ManifestRegistryEntry>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            parse_registry_entry(
                value,
                "managed_config_namespaces",
                idx,
                Some(crate::auth::managed_config::RESOURCE_TYPE_CONFIG_NAMESPACE),
            )
        })
        .collect()
}

fn parse_registry_entry(
    value: &JsonValue,
    field: &str,
    idx: usize,
    forced_type: Option<&str>,
) -> Result<ManifestRegistryEntry, String> {
    let obj = object_at(value, field, idx)?;
    let id = required_string(obj, "id", field, idx)?;
    Ok(ManifestRegistryEntry {
        resource_type: forced_type
            .map(str::to_string)
            .or_else(|| optional_string(obj, "resource_type"))
            .ok_or_else(|| format!("{field}[{idx}].resource_type is required"))?,
        schema: optional_string(obj, "schema").unwrap_or_else(|| "manifest".to_string()),
        mutability: obj
            .get("mutability")
            .and_then(|v| v.as_str())
            .map(parse_mutability)
            .transpose()?
            .unwrap_or(Mutability::Immutable),
        sensitivity: obj
            .get("sensitivity")
            .and_then(|v| v.as_str())
            .map(parse_sensitivity)
            .transpose()?
            .unwrap_or(Sensitivity::Internal),
        managed: optional_bool(obj, "managed").unwrap_or(true),
        required_action: optional_string(obj, "required_action")
            .unwrap_or_else(|| "config:write".to_string()),
        required_resource: optional_string(obj, "required_resource")
            .unwrap_or_else(|| format!("config:{id}")),
        evidence: obj
            .get("evidence")
            .and_then(|v| v.as_str())
            .map(parse_evidence)
            .transpose()?
            .unwrap_or(EvidenceRequirement::Metadata),
        id,
    })
}

fn parse_config(values: &[JsonValue]) -> Result<Vec<ManifestConfig>, String> {
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let obj = object_at(value, "config", idx)?;
            let key = required_string(obj, "key", "config", idx)?;
            if key.trim().is_empty() {
                return Err(format!("config[{idx}].key is required"));
            }
            if obj.contains_key("secret") || obj.contains_key("plaintext") {
                return Err(format!(
                    "config[{idx}] must not contain secret plaintext; use secret_ref"
                ));
            }
            let value = if let Some(secret_ref) = obj.get("secret_ref") {
                secret_ref_storage_value(secret_ref, idx)?
            } else {
                json_to_storage_value(
                    obj.get("value")
                        .ok_or_else(|| format!("config[{idx}].value or secret_ref is required"))?,
                )?
            };
            Ok(ManifestConfig { key, value })
        })
        .collect()
}

fn validate_registry_authorization_plan(
    users: &[ManifestUser],
    policies: &[Policy],
    managed_policies: &[ManagedPolicy],
    attachments: &[PolicyAttachment],
    registry_entries: &[ManifestRegistryEntry],
    managed_config_namespaces: &[ManifestRegistryEntry],
    actor: &str,
) -> Result<(), String> {
    let needs_registry = !registry_entries.is_empty()
        || !managed_policies.is_empty()
        || !managed_config_namespaces.is_empty();
    if !needs_registry {
        return Ok(());
    }

    let actor_user = users
        .iter()
        .find(|user| user.tenant.is_none() && user.username == actor)
        .ok_or_else(|| format!("manifest actor `{actor}` must be declared as a platform user"))?;
    let ctx = manifest_user_context(actor_user);
    let policy_by_id: HashMap<&str, &Policy> = policies
        .iter()
        .chain(managed_policies.iter().map(|managed| &managed.policy))
        .map(|policy| (policy.id.as_str(), policy))
        .collect();
    let actor_policies: Vec<&Policy> = attachments
        .iter()
        .filter(|attachment| attachment.user.as_deref() == Some(actor))
        .filter_map(|attachment| policy_by_id.get(attachment.policy.as_str()).copied())
        .collect();

    let mut entry_ids: Vec<&str> = registry_entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    entry_ids.extend(
        managed_policies
            .iter()
            .map(|managed| managed.policy.id.as_str()),
    );
    entry_ids.extend(
        managed_config_namespaces
            .iter()
            .map(|entry| entry.id.as_str()),
    );

    for id in entry_ids {
        let resource = ResourceRef::new(RESOURCE_KIND, id);
        if !matches!(
            evaluate(&actor_policies, ACTION_REGISTER, &resource, &ctx),
            Decision::Allow { .. }
        ) {
            return Err(format!(
                "manifest actor `{actor}` must have an attached policy allowing \
                 {ACTION_REGISTER} on {RESOURCE_KIND}:{id}"
            ));
        }
    }
    Ok(())
}

fn register_entry(
    registry: &ConfigRegistry,
    auth: &AuthStore,
    actor: &UserId,
    ctx: &EvalContext,
    entry: &ManifestRegistryEntry,
    now_ms: u128,
) -> Result<ConfigRegistryEntry, String> {
    registry
        .register(
            auth,
            actor,
            ctx,
            ConfigRegistryDraft {
                id: entry.id.clone(),
                resource_type: entry.resource_type.clone(),
                schema: entry.schema.clone(),
                mutability: entry.mutability,
                sensitivity: entry.sensitivity,
                managed: entry.managed,
                required_action: entry.required_action.clone(),
                required_resource: entry.required_resource.clone(),
                evidence_requirement: entry.evidence,
            },
            now_ms,
        )
        .map_err(|err| format!("register `{}`: {err}", entry.id))
}

pub(crate) fn persist_registry_state(
    runtime: &RedDBRuntime,
    entries: &[ConfigRegistryEntry],
) -> Result<(), String> {
    let json = JsonValue::Array(entries.iter().map(registry_entry_to_json).collect());
    insert_config_value(
        runtime,
        REGISTRY_STATE_KEY,
        Value::Json(
            crate::serde_json::to_vec(&json)
                .map_err(|err| format!("serialize bootstrap registry state: {err}"))?,
        ),
    )
}

fn registry_entry_to_json(entry: &ConfigRegistryEntry) -> JsonValue {
    let mut obj = JsonMap::new();
    obj.insert("id".to_string(), JsonValue::String(entry.id.clone()));
    obj.insert(
        "version".to_string(),
        JsonValue::Number(entry.version as f64),
    );
    obj.insert(
        "resource_type".to_string(),
        JsonValue::String(entry.resource_type.clone()),
    );
    obj.insert(
        "schema".to_string(),
        JsonValue::String(entry.schema.clone()),
    );
    obj.insert(
        "mutability".to_string(),
        JsonValue::String(mutability_str(entry.mutability).to_string()),
    );
    obj.insert(
        "sensitivity".to_string(),
        JsonValue::String(sensitivity_str(entry.sensitivity).to_string()),
    );
    obj.insert("managed".to_string(), JsonValue::Bool(entry.managed));
    obj.insert(
        "required_action".to_string(),
        JsonValue::String(entry.required_action.clone()),
    );
    obj.insert(
        "required_resource".to_string(),
        JsonValue::String(entry.required_resource.clone()),
    );
    obj.insert(
        "evidence".to_string(),
        JsonValue::String(evidence_str(entry.evidence_requirement).to_string()),
    );
    obj.insert(
        "updated_by".to_string(),
        JsonValue::String(entry.updated_by.clone()),
    );
    obj.insert(
        "updated_at_ms".to_string(),
        JsonValue::Number(entry.updated_at_ms as f64),
    );
    JsonValue::Object(obj)
}

fn registry_entry_from_json(value: &JsonValue, idx: usize) -> Result<ConfigRegistryEntry, String> {
    let obj = object_at(value, "registry_state", idx)?;
    Ok(ConfigRegistryEntry {
        id: required_string(obj, "id", "registry_state", idx)?,
        version: obj
            .get("version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("registry_state[{idx}].version is required"))?,
        resource_type: required_string(obj, "resource_type", "registry_state", idx)?,
        schema: required_string(obj, "schema", "registry_state", idx)?,
        mutability: parse_mutability(&required_string(obj, "mutability", "registry_state", idx)?)?,
        sensitivity: parse_sensitivity(&required_string(
            obj,
            "sensitivity",
            "registry_state",
            idx,
        )?)?,
        managed: obj
            .get("managed")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| format!("registry_state[{idx}].managed is required"))?,
        required_action: required_string(obj, "required_action", "registry_state", idx)?,
        required_resource: required_string(obj, "required_resource", "registry_state", idx)?,
        evidence_requirement: parse_evidence(&required_string(
            obj,
            "evidence",
            "registry_state",
            idx,
        )?)?,
        updated_by: required_string(obj, "updated_by", "registry_state", idx)?,
        updated_at_ms: obj
            .get("updated_at_ms")
            .and_then(|v| v.as_u64())
            .map(u128::from)
            .ok_or_else(|| format!("registry_state[{idx}].updated_at_ms is required"))?,
    })
}

/// Write an initial config value only when the key is absent (issue #1232,
/// acceptance #3). The fenced bootstrap owner applies the manifest exactly
/// once, but initial config must use first-boot/write-if-absent semantics so
/// a value an operator has already set for the same key is never overwritten.
/// Internal bootstrap bookkeeping (e.g. the registry snapshot) keeps using
/// [`insert_config_value`] directly, because that state is owned by the
/// bootstrap path itself, not by the operator.
fn insert_config_value_if_absent(
    runtime: &RedDBRuntime,
    key: &str,
    value: Value,
) -> Result<(), String> {
    if runtime.db().store().get_config(key).is_some() {
        tracing::info!(
            key,
            "bootstrap manifest config key already present; preserving operator value"
        );
        return Ok(());
    }
    insert_config_value(runtime, key, value)
}

fn insert_config_value(runtime: &RedDBRuntime, key: &str, value: Value) -> Result<(), String> {
    let store = runtime.db().store();
    let _ = store.get_or_create_collection("red_config");
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from("red_config"),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(
                [
                    ("key".to_string(), Value::text(key.to_string())),
                    ("value".to_string(), value),
                ]
                .into_iter()
                .collect::<HashMap<_, _>>(),
            ),
            schema: None,
        }),
    );
    store
        .insert_auto("red_config", entity)
        .map(|_| ())
        .map_err(|err| format!("persist config `{key}`: {err}"))
}

fn registry_context(user: &User) -> EvalContext {
    EvalContext {
        principal_tenant: user.tenant_id.clone(),
        current_tenant: user.tenant_id.clone(),
        peer_ip: None,
        mfa_present: false,
        now_ms: current_unix_ms(),
        principal_is_admin_role: user.role == Role::Admin,
        principal_is_platform_scoped: user.tenant_id.is_none(),
    }
}

fn manifest_user_context(user: &ManifestUser) -> EvalContext {
    EvalContext {
        principal_tenant: user.tenant.clone(),
        current_tenant: user.tenant.clone(),
        peer_ip: None,
        mfa_present: false,
        now_ms: current_unix_ms(),
        principal_is_admin_role: user.role == Role::Admin,
        principal_is_platform_scoped: user.tenant.is_none(),
    }
}

fn user_id(tenant: &Option<String>, username: &str) -> UserId {
    UserId::from_parts(tenant.as_deref(), username)
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn array_field<'a>(
    obj: &'a JsonMap<String, JsonValue>,
    name: &str,
) -> Result<&'a [JsonValue], String> {
    match obj.get(name) {
        None => Ok(&[]),
        Some(JsonValue::Array(values)) => Ok(values.as_slice()),
        Some(_) => Err(format!(
            "bootstrap manifest field `{name}` must be an array"
        )),
    }
}

fn object_at<'a>(
    value: &'a JsonValue,
    field: &str,
    idx: usize,
) -> Result<&'a JsonMap<String, JsonValue>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{field}[{idx}] must be an object"))
}

fn required_string(
    obj: &JsonMap<String, JsonValue>,
    key: &str,
    field: &str,
    idx: usize,
) -> Result<String, String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{field}[{idx}].{key} is required"))
}

fn optional_string(obj: &JsonMap<String, JsonValue>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn optional_bool(obj: &JsonMap<String, JsonValue>, key: &str) -> Option<bool> {
    obj.get(key).and_then(|v| v.as_bool())
}

fn parse_mutability(value: &str) -> Result<Mutability, String> {
    match value {
        "immutable" => Ok(Mutability::Immutable),
        "mutable_via_governance" => Ok(Mutability::MutableViaGovernance),
        _ => Err(format!("unknown registry mutability `{value}`")),
    }
}

fn mutability_str(value: Mutability) -> &'static str {
    match value {
        Mutability::Immutable => "immutable",
        Mutability::MutableViaGovernance => "mutable_via_governance",
    }
}

fn parse_sensitivity(value: &str) -> Result<Sensitivity, String> {
    match value {
        "public" => Ok(Sensitivity::Public),
        "internal" => Ok(Sensitivity::Internal),
        "confidential" => Ok(Sensitivity::Confidential),
        "secret" => Ok(Sensitivity::Secret),
        _ => Err(format!("unknown registry sensitivity `{value}`")),
    }
}

fn sensitivity_str(value: Sensitivity) -> &'static str {
    match value {
        Sensitivity::Public => "public",
        Sensitivity::Internal => "internal",
        Sensitivity::Confidential => "confidential",
        Sensitivity::Secret => "secret",
    }
}

fn parse_evidence(value: &str) -> Result<EvidenceRequirement, String> {
    match value {
        "none" => Ok(EvidenceRequirement::None),
        "metadata" => Ok(EvidenceRequirement::Metadata),
        "full" => Ok(EvidenceRequirement::Full),
        _ => Err(format!("unknown registry evidence requirement `{value}`")),
    }
}

fn evidence_str(value: EvidenceRequirement) -> &'static str {
    match value {
        EvidenceRequirement::None => "none",
        EvidenceRequirement::Metadata => "metadata",
        EvidenceRequirement::Full => "full",
    }
}

fn json_to_storage_value(value: &JsonValue) -> Result<Value, String> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Boolean(*value),
        JsonValue::Integer(value) => {
            if *value >= 0 {
                Value::UnsignedInteger(*value as u64)
            } else {
                Value::Integer(*value)
            }
        }
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON && *value >= 0.0 {
                Value::UnsignedInteger(*value as u64)
            } else if value.fract().abs() < f64::EPSILON {
                Value::Integer(*value as i64)
            } else {
                Value::Float(*value)
            }
        }
        JsonValue::String(value) => Value::text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => Value::Json(
            crate::serde_json::to_vec(value)
                .map_err(|err| format!("serialize config JSON value: {err}"))?,
        ),
    })
}

fn secret_ref_storage_value(value: &JsonValue, idx: usize) -> Result<Value, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("config[{idx}].secret_ref must be an object"))?;
    let collection = required_string(obj, "collection", "config.secret_ref", idx)?;
    let key = required_string(obj, "key", "config.secret_ref", idx)?;
    let mut out = JsonMap::new();
    out.insert(
        "type".to_string(),
        JsonValue::String("secret_ref".to_string()),
    );
    out.insert("store".to_string(), JsonValue::String("vault".to_string()));
    out.insert("collection".to_string(), JsonValue::String(collection));
    out.insert("key".to_string(), JsonValue::String(key));
    Ok(Value::Json(
        crate::serde_json::to_vec(&JsonValue::Object(out))
            .map_err(|err| format!("serialize config[{idx}].secret_ref: {err}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{apply_manifest_file, BootstrapManifest};
    use crate::auth::store::AuthStore;
    use crate::auth::AuthConfig;
    use crate::storage::schema::Value;
    use crate::RedDBRuntime;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn manifest_test_env() -> (RedDBRuntime, Arc<AuthStore>, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let auth = Arc::new(AuthStore::new(AuthConfig::default()));
        let tmp =
            std::env::temp_dir().join(format!("reddb_manifest_{}_{}.json", std::process::id(), id));
        (runtime, auth, tmp)
    }

    fn write_manifest(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).expect("write manifest");
    }

    #[test]
    fn bootstrap_intent_manifest_reads_password_files() {
        let (_runtime, _auth, path) = manifest_test_env();
        let dir = path.parent().expect("manifest parent");
        let head_password = dir.join(format!("head-secret-{}", std::process::id()));
        let customer_password = dir.join(format!("customer-secret-{}", std::process::id()));
        std::fs::write(&head_password, "head-pass\n").expect("write head secret");
        std::fs::write(&customer_password, "customer-pass\r\n").expect("write customer secret");

        write_manifest(
            &path,
            &format!(
                r#"{{
                    "bootstrap": {{
                        "preset": "cloud",
                        "cloud_head_admin": {{
                            "username": "head",
                            "password_file": "{}"
                        }},
                        "customer_admin": {{
                            "username": "customer",
                            "password_file": "{}"
                        }}
                    }}
                }}"#,
                head_password.display(),
                customer_password.display()
            ),
        );

        let config = super::bootstrap_config_from_manifest_file(&path)
            .expect("parse intent manifest")
            .expect("intent present");
        assert_eq!(config.preset.as_deref(), Some("cloud"));
        assert_eq!(config.cloud_head_admin.as_deref(), Some("head"));
        assert_eq!(
            config.cloud_head_admin_password.as_deref(),
            Some("head-pass")
        );
        assert_eq!(config.customer_admin.as_deref(), Some("customer"));
        assert_eq!(
            config.customer_admin_password.as_deref(),
            Some("customer-pass")
        );
        assert!(
            config.manifest.is_none(),
            "intent manifests must not recurse back into manifest apply"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&head_password);
        let _ = std::fs::remove_file(&customer_password);
    }

    #[test]
    fn bootstrap_intent_manifest_rejects_inline_plaintext_secret() {
        let (_runtime, _auth, path) = manifest_test_env();
        write_manifest(
            &path,
            r#"{
                "bootstrap": {
                    "preset": "production",
                    "admin": {
                        "username": "ops",
                        "password": "inline"
                    }
                }
            }"#,
        );

        let err = super::bootstrap_config_from_manifest_file(&path)
            .expect_err("inline manifest secret must be rejected");
        assert!(err.contains("password_file"), "got: {err}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn owner_apply_creates_user_and_initial_config() {
        // Acceptance #1/#4: the fenced owner applies the manifest, creating the
        // initial admin and writing initial config.
        let (runtime, auth, path) = manifest_test_env();
        write_manifest(
            &path,
            r#"{
                "users": [{"username":"ops","password":"hunter2","role":"admin"}],
                "config": [{"key":"app.feature.x","value":"on"}]
            }"#,
        );

        let actor =
            apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path).expect("apply");
        assert_eq!(actor, "ops");
        assert!(
            auth.get_user(None, "ops").is_some(),
            "admin must be created"
        );
        assert_eq!(
            runtime.db().store().get_config("app.feature.x"),
            Some(Value::text("on".to_string())),
            "initial config must be written on owner apply"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn owner_apply_reads_user_password_file() {
        let (runtime, auth, path) = manifest_test_env();
        let password_path = path.with_extension("secret");
        std::fs::write(&password_path, "hunter2\n").expect("write secret");
        write_manifest(
            &path,
            &format!(
                r#"{{
                    "users": [
                        {{
                            "username": "ops",
                            "password_file": "{}",
                            "role": "admin"
                        }}
                    ]
                }}"#,
                password_path.display()
            ),
        );

        apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path).expect("apply");
        auth.authenticate("ops", "hunter2")
            .expect("password must come from mounted file");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&password_path);
    }

    #[test]
    fn initial_config_is_write_if_absent_and_keeps_operator_value() {
        // Acceptance #3: initial config uses write-if-absent semantics — a value
        // an operator has already set for the same key survives manifest apply.
        let (runtime, auth, path) = manifest_test_env();
        super::insert_config_value(
            &runtime,
            "app.feature.x",
            Value::text("operator".to_string()),
        )
        .expect("seed operator config");

        write_manifest(
            &path,
            r#"{
                "users": [{"username":"ops","password":"hunter2","role":"admin"}],
                "config": [{"key":"app.feature.x","value":"manifest"}]
            }"#,
        );
        apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path).expect("apply");

        assert_eq!(
            runtime.db().store().get_config("app.feature.x"),
            Some(Value::text("operator".to_string())),
            "manifest must not overwrite a later operator change"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duplicate_manifest_apply_is_rejected_idempotently() {
        // Acceptance #4: a duplicate apply (e.g. a restart that re-runs the
        // manifest) refuses to recreate existing global auth state instead of
        // silently mutating it.
        let (runtime, auth, path) = manifest_test_env();
        write_manifest(
            &path,
            r#"{
                "users": [{"username":"ops","password":"hunter2","role":"admin"}],
                "config": [{"key":"app.feature.x","value":"on"}]
            }"#,
        );
        apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path).expect("first");

        let err = apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path)
            .expect_err("duplicate apply must be rejected");
        assert!(err.contains("already exists"), "got: {err}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cloud_policy_first_bootstrap_protects_cloud_admin_lifecycle() {
        // Issue #1233: prove the RedDB Cloud first-boot model on the bootstrap
        // manifest path. The cloud/head admin and the customer admin are
        // ordinary policy-governed users — broad admin authority is allowed,
        // but an explicit deny fences the customer admin out of the protected
        // cloud admin lifecycle operations.
        use crate::auth::{Role, UserId};

        let (runtime, auth, path) = manifest_test_env();
        write_manifest(
            &path,
            r#"{
                "actor": "cloud-admin",
                "users": [
                    {"username":"cloud-admin","password":"head-secret","role":"admin"},
                    {"username":"customer-admin","password":"tenant-secret","role":"admin"}
                ],
                "policies": [
                    {
                        "id": "customer-admin-allow-all",
                        "version": 1,
                        "statements": [
                            {"effect":"allow","actions":["*"],"resources":["*"]}
                        ]
                    },
                    {
                        "id": "cloud-admin-protection",
                        "version": 1,
                        "statements": [
                            {
                                "effect":"deny",
                                "actions":[
                                    "user:delete",
                                    "user:disable",
                                    "user:password:change",
                                    "user:role:update"
                                ],
                                "resources":["user:cloud-admin"]
                            }
                        ]
                    }
                ],
                "attachments": [
                    {"user":"customer-admin","policy":"customer-admin-allow-all"},
                    {"user":"customer-admin","policy":"cloud-admin-protection"}
                ]
            }"#,
        );

        let actor =
            apply_manifest_file(&runtime, &auth, &runtime.config_registry(), &path).expect("apply");
        assert_eq!(actor, "cloud-admin");

        // Acceptance #1: both admins exist as ordinary, policy-governed users.
        assert!(
            auth.get_user(None, "cloud-admin").is_some(),
            "cloud/head admin must be created"
        );
        assert!(
            auth.get_user(None, "customer-admin").is_some(),
            "customer admin must be created"
        );

        let customer = UserId::platform("customer-admin");
        let cloud_admin = UserId::platform("cloud-admin");

        // Acceptance #3: the customer admin is denied the protected lifecycle
        // operations on the cloud admin resource.
        for action in [
            "user:delete",
            "user:disable",
            "user:password:change",
            "user:role:update",
        ] {
            assert!(
                !auth.check_user_lifecycle_authz(&customer, Role::Admin, action, &cloud_admin),
                "customer admin must be denied {action} on the cloud admin"
            );
        }

        // Acceptance #2: the customer admin keeps broad admin authority outside
        // the protected cloud admin lifecycle — the same operations succeed
        // against any other user.
        let other = UserId::platform("some-tenant-user");
        for action in [
            "user:delete",
            "user:disable",
            "user:password:change",
            "user:role:update",
        ] {
            assert!(
                auth.check_user_lifecycle_authz(&customer, Role::Admin, action, &other),
                "customer admin must retain {action} on non-protected users"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_policy_condition_system_owned() {
        // Acceptance #4: policies in a manifest still reject the retired
        // `condition.system_owned` key — protection is expressed with explicit
        // resource-scoped deny policies, not a magic system-owned flag.
        let result = BootstrapManifest::parse(
            r#"{
                "users": [{"username":"cloud-admin","password":"head","role":"admin"}],
                "policies": [{
                    "id": "p-sys",
                    "version": 1,
                    "statements": [{
                        "effect": "allow",
                        "actions": ["admin:reload"],
                        "resources": ["*"],
                        "condition": { "system_owned": true }
                    }]
                }]
            }"#,
        );

        match result {
            Ok(_) => panic!("manifest accepted policy condition.system_owned"),
            Err(err) => assert!(err.contains("system_owned"), "got: {err}"),
        }
    }

    #[test]
    fn rejects_system_owned_user_field() {
        let result = BootstrapManifest::parse(
            r#"{
                "users": [
                    {
                        "username": "ops",
                        "password": "hunter2",
                        "role": "admin",
                        "system_owned": true
                    }
                ]
            }"#,
        );

        match result {
            Ok(_) => panic!("manifest accepted users[0].system_owned"),
            Err(err) => assert!(err.contains("users[0].system_owned"), "got: {err}"),
        }
    }
}
