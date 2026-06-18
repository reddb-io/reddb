# Control Evidence Matrix

This page maps RedDB product capabilities to audit evidence customers can use
when operating governed or regulated workloads. It is capability-first: it
starts from what RedDB can prove, then shows which external audit programs
commonly ask for similar evidence.

This is controls and evidence support, not automatic compliance certification.
Deploying RedDB does not automatically make an organization compliant.
Auditors evaluate the operator's people, processes, deployment, retention,
access reviews, incident handling, and vendor controls in addition to database
features.

## Evidence principles

RedDB evidence surfaces should follow these rules:

| Principle | Meaning |
|---|---|
| Durable | Evidence survives process restart and is protected by WAL/backup posture when durable mode is configured. |
| Attributable | Events identify the actor, tenant/scope, action, resource, outcome, and request/trace id when available. |
| Deny-aware | Blocked attempts are evidence too. A denied policy/config/vault mutation belongs in the same timeline as allowed actions. |
| Minimal | Evidence stores safe metadata and fingerprints by default. Raw query text is absent by default. Secret plaintext is absent by default. |
| Queryable | Operators and auditors can query implemented evidence through `red.*` surfaces instead of parsing implementation logs. |
| Exportable | Evidence can be exported with filters and integrity metadata for an audit period. |

## Current implemented and tested foundations

Only implemented and tested surfaces are listed here as current foundations.
Future dedicated evidence views are listed under required capabilities instead
of being presented as already available.

| Area | Current implemented and tested foundation | Evidence |
|---|---|---|
| Governance catalog | `red.registry`, `red.registry_history`, `red.managed_policies`, and `red.control_capabilities` expose managed policy/config metadata and capability vocabulary. | `tests/e2e_red_schema.rs`, `crates/reddb-server/src/service_cli.rs` regulated preset tests |
| User and credential evidence metadata | `red.users` and `red.api_keys` expose minimized user and API-key metadata without password hashes, API-key plaintext, or secret values. | `tests/e2e_red_schema.rs` |
| Control Event Ledger | `red.control_events` records implemented low-volume control-plane evidence with actor, scope, action, resource, outcome, policy match data when available, and minimized fields. | `tests/e2e_red_schema.rs`, `tests/e2e_control_events_operational.rs`, `crates/reddb-server/tests/control_events_policy.rs` |
| Policy lifecycle events | Runtime policy DDL and AuthStore audited policy mutation siblings emit allowed, denied, and error evidence for policy create/update/delete and attach/detach paths. | `crates/reddb-server/tests/control_events_policy.rs` |
| Managed guardrails | Managed policies and managed config namespaces are protected through the registry, and denied mutation attempts are recorded before returning the guardrail error. | `crates/reddb-server/src/service_cli.rs` regulated preset tests |
| Vault and secret control events | Vault metadata reads, unseal attempts, rotation, and purge are recorded in `red.control_events` with fingerprints and versions instead of raw secret material. | `tests/e2e_vault_sealed_storage.rs` |
| Schema, tenant, RLS, and backup control events | Implemented DDL, tenant governance, RLS policy, truncate, and backup actions record allowed, denied, and error outcomes in `red.control_events`. | `tests/e2e_control_events_operational.rs` |
| Query audit by scope | `red.query_audit` records metadata-only query audit rows when configured by actor, tenant, collection, and action. The default row contains statement kind, touched collections, duration, row count, request id, and query hash, not raw query text. | `tests/e2e_query_audit.rs` |
| Evidence export | `export_evidence` returns filtered control-event reports with counts, high-water marks, per-event hashes, and export integrity hashes, and records allowed/denied exports in `red.control_events`. | `tests/e2e_evidence_export.rs` |
| Bootstrap presets | `simple`, `production`, `regulated`, and `cloud` presets are wired through `REDDB_BOOTSTRAP_PRESET` (`REDDB_PRESET` remains a compatibility alias); bootstrap manifests can seed users, policies, attachments, managed guardrails, and config. | `crates/reddb-server/src/service_cli.rs` preset and manifest tests |

## Required evidence surfaces

These are the product surfaces RedDB should expose to make audit support
coherent. The "Current foundation" column only names implemented and tested
surfaces. The "Required capability" column keeps required but unimplemented
surfaces visible.

| RedDB capability | Current foundation | Required capability | Typical evidence questions | SOC 2 | ISO 27001 | HIPAA | PCI DSS | GDPR/LGPD |
|---|---|---|---|---:|---:|---:|---:|---:|
| User lifecycle | `red.users` minimized metadata | Complete user lifecycle producer coverage in `red.control_events` for create, disable, delete, and role/scope changes | Who created, disabled, deleted, or changed a user? When? Under which scope? | Yes | Yes | Yes | Yes | Supports accountability |
| Password and API-key lifecycle | `red.users`, `red.api_keys` minimized metadata | Complete credential lifecycle producer coverage in `red.control_events` for password change and API-key create/rotate/revoke/failure paths | When was the credential created, rotated, revoked, or failed? Who did it? | Yes | Yes | Yes | Yes | Supports access control |
| Policy lifecycle | `red.policies`, `red.managed_policies`, `red.registry`, `red.registry_history`, `red.control_events` | Extend audited mutation coverage to any remaining non-runtime/bootstrap-internal policy mutation path without double-emitting | Who changed access rules? What policy/version/hash changed? Was it allowed or denied? | Yes | Yes | Yes | Yes | Supports least privilege |
| Managed policies | `red.managed_policies`, `red.registry`, `red.control_events` | None for the current managed-policy guardrail foundation | Which guardrails are operator-owned? Who attempted to change them? | Yes | Yes | Yes | Yes | Supports processor/operator separation |
| Managed config namespaces | `red.registry`, `red.registry_history`, `red.control_capabilities`, `red.control_events` for guarded writes | Dedicated `red.config_events` view and broader live-config mutation taxonomy | Which config keys are protected? Who changed or attempted to change them? | Yes | Yes | Yes | Yes | Supports governance |
| Vault metadata and unseal | `red.control_events` for metadata read, unseal, rotate, and purge | Dedicated `red.vault_metadata` and `red.secret_events` views | Who unsealed, read metadata, rotated, purged, or failed to access a secret? | Yes | Yes | Yes | Yes | Supports confidentiality |
| Schema and DDL changes | `red.control_events` for implemented DDL, tenant governance, RLS, and truncate events | Dedicated `red.schema_events`, `red.tenant_events`, and `red.policy_events` views | Who created, altered, dropped, truncated, or changed retention on a collection? | Yes | Yes | Yes | Yes | Supports data governance and separation |
| Backup, restore, and PITR | `red.control_events` for backup trigger metadata | Dedicated `red.backup_events` and `red.restore_events` views, plus restore/PITR producer coverage | When was backup/restore/PITR run? Which snapshot/WAL hash chain was used? | Yes | Yes | Yes | Yes | Supports resilience |
| Failover and replication | Event kinds are reserved in the ledger vocabulary | `red.replication_events` view and failover/replication producers for allowed, denied, and refused promotions | Who promoted a replica? What lag/lease state existed? Was promotion refused? | Yes | Yes | Indirect | Indirect | Indirect |
| Runtime config changes | `red.control_events` for implemented guarded config writes | Dedicated `red.config_events` view and full runtime config mutation producer coverage | Who changed live settings? Was the key sensitive or managed? | Yes | Yes | Yes | Yes | Supports governance |
| Query audit by scope | `red.query_audit` scoped metadata stream | Optional explicit raw-query capture controls, retention policy, and operator-facing rule management | Which actor touched sensitive collections? What operation, duration, row count, and query hash? | Optional | Optional | Often useful | Often useful | Supports access logging |
| Evidence export | `export_evidence` API/report and export control events | Dedicated `red.evidence_exports` relation for persisted export manifests | Who exported evidence? For which time window and filters? What integrity metadata proves completeness? | Yes | Yes | Yes | Yes | Supports audit requests |

## Presets

RedDB exposes four bootstrap presets through `REDDB_BOOTSTRAP_PRESET`
(`REDDB_PRESET` remains accepted when the canonical env var is unset):

| Preset | Purpose | Evidence behavior |
|---|---|---|
| `simple` | Default low-friction bootstrap for local development and small deployments. | Persists bootstrap idempotency state only. The simple preset does not enable regulated evidence overhead, query-audit infrastructure, or fail-closed control-event persistence by itself. |
| `production` | Creates the first platform-scoped, system-owned admin from `REDDB_USERNAME`/`REDDB_PASSWORD` or their `_FILE` companions and grants authority through an attached allow-all policy. | Uses policy-derived authority rather than an admin bypass and persists bootstrap state for idempotent restarts. |
| `regulated` | Enables evidence guardrails for regulated workloads without globally auditing data-plane queries. | Enables fail-closed control-event persistence, creates query-audit infrastructure with no rules, installs managed evidence guardrail policy/config registry entries, and records denied guardrail mutations. |
| `cloud` | Creates a platform-scoped, system-owned head admin plus an ordinary platform customer admin from `REDDB_CLOUD_HEAD_ADMIN*` and `REDDB_CUSTOMER_ADMIN*` credentials. | Enables auth, require-auth, and vault by default, attaches allow-all policy-derived authority, installs cloud managed guardrails, and relies on system-owned immutability so the customer admin cannot delete or mutate the head admin. |

Bootstrap manifests remain compatible with the preset path. A manifest can seed
initial users, policies, policy attachments, managed policies, managed config
namespaces, and config values on first boot. After first boot, persisted
bootstrap and registry state make restart idempotent even if the manifest file
is no longer present.

## Required capabilities still open

### User and credential lifecycle producers

`red.users` and `red.api_keys` are current minimized metadata surfaces. RedDB
still needs complete lifecycle producers that record create, disable, delete,
password change, API-key creation, API-key rotation, API-key revocation, and
credential failure events in `red.control_events`.

### Dedicated evidence views

The ledger currently stores several implemented events in `red.control_events`.
Dedicated, query-friendly views remain required for domains where auditors may
want focused timelines:

- `red.vault_metadata`
- `red.secret_events`
- `red.schema_events`
- `red.tenant_events`
- `red.policy_events`
- `red.backup_events`
- `red.restore_events`
- `red.replication_events`
- `red.config_events`
- `red.evidence_exports`

### Restore, PITR, failover, and replication producers

Backup-trigger control events are current. Restore/PITR and failover/replication
producers still need to emit allowed, denied, refused, and error outcomes with
snapshot ids, WAL high-water marks, hash-chain metadata, promotion lease state,
and lag evidence.

### Query audit rule management

Scoped metadata-only query audit is current. RedDB still needs public rule
management and retention controls, plus an explicit opt-in path for raw-query
capture that documents the PII risk and keeps raw query text disabled by
default.

## Non-goals

- RedDB does not certify a customer's organization for SOC 2, ISO 27001, HIPAA,
  PCI DSS, GDPR, or LGPD.
- RedDB does not globally record raw query text by default.
- RedDB does not store secret plaintext, password plaintext, tokens, private
  keys, certificates, or raw secret values in evidence payloads.
- RedDB does not require regulated evidence features for the default simple
  preset.
