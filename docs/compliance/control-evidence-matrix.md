# Control Evidence Matrix

This page maps RedDB product capabilities to audit evidence customers can use
when operating regulated workloads. It is capability-first: it starts from what
RedDB can prove, then shows which external audit programs commonly ask for
similar evidence.

This page is not a certification, legal checklist, or promise that deploying
RedDB automatically makes an organization compliant. Auditors evaluate the
operator's people, processes, deployment, retention, access reviews, incident
handling, and vendor controls in addition to database features.

## Evidence principles

RedDB evidence surfaces should follow these rules:

| Principle | Meaning |
|---|---|
| Durable | Evidence survives process restart and is protected by WAL/backup posture. |
| Attributable | Events identify the actor, tenant/scope, action, resource, outcome, and request/trace id when available. |
| Deny-aware | Blocked attempts are evidence too. A denied policy/config/vault mutation belongs in the same timeline as allowed actions. |
| Minimal | Evidence stores safe metadata and fingerprints by default, not passwords, secret values, private keys, tokens, or raw query text. |
| Queryable | Operators and auditors can query evidence through `red.*` surfaces instead of parsing implementation logs. |
| Exportable | Evidence can be exported with filters and integrity metadata for an audit period. |

## Current foundation

These pieces already exist or are documented as RedDB foundations:

| Area | Existing foundation |
|---|---|
| Auth and policy | Users, roles, policy documents, policy simulator, tenant-aware users, RLS, and column enforcement are documented in [Auth & Security](../security/overview.md), [Policies](../security/policies.md), and [Permissioning Handbook](../security/permissions.md). |
| System-owned users | The auth store supports users marked `system_owned`; destructive user mutations are rejected while API-key rotation remains allowed. |
| Vault and secrets | The vault stores auth state and secrets under an encrypted seal, documents unseal/restart behavior, rotation, and threat model in [Vault](../security/vault.md). |
| Telemetry channels | Operator-grade events, slow-query logs, admin intent journal, and developer signal are separated in [Logging Operator Guide](../operations/logging.md). |
| Backup and recovery | WAL, snapshots, archive restore, PITR, drills, and RTO/RPO targets are documented in [RTO and RPO](../operations/rto-rpo.md) and [Runbook](../operations/runbook.md). |
| Multi-tenancy | Declarative tenant scoping and RLS patterns are documented in [Multi-Tenancy](../security/multi-tenancy.md). |
| Config and secret model | Config/Vault separation, secret references, and config event intent are documented in [Config, Secrets, and Vault Design](../security/config-secrets-vault-design.md). |

## Required evidence surfaces

These are the product surfaces RedDB should expose to make audit support
coherent. Some are current foundations; some are required capabilities.

| RedDB capability | Evidence surface | Typical evidence questions | SOC 2 | ISO 27001 | HIPAA | PCI DSS | GDPR/LGPD |
|---|---|---|---:|---:|---:|---:|---:|
| User lifecycle | `red.users`, `red.control_events` | Who created, disabled, deleted, or changed a user? When? Under which scope? | Yes | Yes | Yes | Yes | Supports accountability |
| Password and API-key lifecycle | `red.users`, `red.api_keys`, `red.control_events` | When was the credential created, rotated, revoked, or failed? Who did it? | Yes | Yes | Yes | Yes | Supports access control |
| Policy lifecycle | `red.policies`, `red.policy_attachments`, `red.control_events` | Who changed access rules? What policy/version/hash changed? Was it allowed or denied? | Yes | Yes | Yes | Yes | Supports least privilege |
| Managed policies | `red.managed_policies`, `red.control_events` | Which guardrails are operator-owned? Who attempted to change them? | Yes | Yes | Yes | Yes | Supports processor/operator separation |
| Managed config namespaces | `red.config_registry`, `red.control_events` | Which config keys are protected? Who changed or attempted to change them? | Yes | Yes | Yes | Yes | Supports governance |
| Vault metadata and unseal | `red.vault_metadata`, `red.secret_events`, `red.control_events` | Who unsealed, read metadata, rotated, purged, or failed to access a secret? | Yes | Yes | Yes | Yes | Supports confidentiality |
| Schema and DDL changes | `red.schema_events`, `red.control_events` | Who created, altered, dropped, truncated, or changed retention on a collection? | Yes | Yes | Yes | Yes | Supports data governance |
| Tenant and RLS changes | `red.tenant_events`, `red.policy_events`, `red.control_events` | Who changed tenant isolation or row/entity policy? | Yes | Yes | Yes | Yes | Supports data separation |
| Backup, restore, and PITR | `red.backup_events`, `red.restore_events`, `red.control_events` | When was backup/restore/PITR run? Which snapshot/WAL hash chain was used? | Yes | Yes | Yes | Yes | Supports resilience |
| Failover and replication | `red.replication_events`, `red.control_events` | Who promoted a replica? What lag/lease state existed? Was promotion refused? | Yes | Yes | Indirect | Indirect | Indirect |
| Runtime config changes | `red.config_events`, `red.control_events` | Who changed live settings? Was the key sensitive or managed? | Yes | Yes | Yes | Yes | Supports governance |
| Query audit by scope | `red.query_events` | Which actor touched sensitive collections? What operation, duration, row count, and query hash? | Optional | Optional | Often useful | Often useful | Supports access logging |
| Evidence export | `red.evidence_exports` | Who exported evidence? For which time window and filters? What integrity metadata proves completeness? | Yes | Yes | Yes | Yes | Supports audit requests |

## Required capabilities

### Policy-first authorization

RedDB should use one authorization model: users plus policies. `Admin` is a
conventional high-privilege user shape, not a bypass around policy evaluation.
Explicit Deny statements must win over broad Allow policies, including
bootstrap allow-all policies.

Required work:

- Remove or neutralize any authorization path where admin role bypasses policy
  Deny statements.
- Represent initial admin authority as attached policy, not special account
  type.
- Make policy evaluation emit allowed and denied control events.

### Bootstrap presets and manifests

RedDB should support both low-friction and advanced first boot:

- Environment variables for simple bootstrap: preset, first username, password
  or password file.
- Optional bootstrap manifest for declaring initial users, policies, managed
  policies, policy attachments, config preset, and managed config namespaces.
- Presets that install common defaults while keeping all underlying pieces
  configurable.

Required work:

- Define `simple`, `production`, and `regulated` config presets.
- Add bootstrap manifest validation and idempotent first-boot behavior.
- Record first-boot actions in the Control Event Ledger when the ledger is
  enabled by preset/config.

### Managed guardrails

RedDB should support operator-owned guardrails without creating a separate login
principal type.

Required work:

- Add managed-policy metadata.
- Add an internal integrity registry pinning managed policies so removing
  metadata from the policy document does not unlock it.
- Add managed config namespace metadata and enforcement.
- Ensure ordinary users, including allow-all users, cannot mutate protected
  guardrails unless a policy explicitly allows it and the caller satisfies the
  system-owned/platform-scoped conditions.

### Control Event Ledger

RedDB should expose a canonical append-only ledger for low-volume control-plane
evidence.

Required work:

- Define `red.control_events` with safe metadata fields.
- Record allowed, denied, and error outcomes in the same ledger.
- Store policy match data (`matched_policy_id`, `matched_sid`, decision reason)
  where applicable.
- Store fingerprints/diffs for sensitive changes rather than raw sensitive
  payloads.
- Fail closed for sensitive control-plane actions when Compliance Mode requires
  durable evidence and the ledger cannot persist.

### Query audit by scope

Query audit is separate from the Control Event Ledger because it is
high-volume, potentially sensitive, and workload-dependent.

Required work:

- Define scoped query-audit rules by actor, tenant, collection, action, and data
  classification.
- Default to metadata-only records: actor, tenant, statement kind, touched
  collections, duration, row counts, request id, and query hash.
- Require explicit configuration to store raw query text, and document the PII
  risk clearly.
- Enable query-audit infrastructure in regulated presets without globally
  auditing every query by default.

### Evidence export

RedDB should make audit response repeatable.

Required work:

- Add filtered export for a time window, actor, tenant, resource, capability,
  or framework mapping.
- Include export metadata: filters, start/end timestamps, event counts, ledger
  high-water marks, and integrity hashes.
- Record every export as a Control Event.

## Non-goals

- RedDB does not certify a customer's organization for SOC 2, ISO 27001, HIPAA,
  PCI DSS, GDPR, or LGPD.
- RedDB does not globally record raw query text by default.
- RedDB does not store secret plaintext, password plaintext, tokens, private
  keys, or certificates in evidence payloads.
- RedDB does not require regulated evidence features for the default simple
  preset.
