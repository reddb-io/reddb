# ADR 0058 - Cluster bootstrap authority

Status: accepted
Date: 2026-06-19

Resolves issue #1228 (parent #1227). Extends
[ADR 0021](0021-policy-first-authorization.md),
[ADR 0022](0022-red-registry-managed-guardrails.md),
[ADR 0037](0037-shard-range-ownership-catalog.md), and
[ADR 0052](0052-cluster-supervisor-control-plane-consensus.md).

RedDB cluster mode needs one authority for global auth, vault, config, and
policy first boot. Symmetric members must not each decide that they are the
first writer, because that can create divergent admins, vault material, policy
attachments, or bootstrap-complete markers. This ADR chooses the authority model
future cluster auth/vault implementation must follow.

## Decision

**Cluster first boot uses the reserved global system range owner model.** RedDB
reserves one global system range as the single authority domain for global auth,
vault, config, policy, and bootstrap completion. The current reserved global
system range owner is the only node allowed to create the initial vault, create
initial admins, install bootstrap policies, apply the first operator/cloud
manifest, or mark bootstrap complete.

**Global state lives in the reserved global system range.** The reserved range
stores global auth, vault, config, and policy state plus the durable
bootstrap-complete marker. The marker is the cluster-wide
`system.bootstrap.completed` fact for clustered deployments; no per-node marker,
local volume marker, router cache, or Supervisor-only memory state is an
authority for global bootstrap completion.

**The owner is fenced by lease/term and ownership epoch.** A node may perform
cluster bootstrap only while it is the current lease/term owner of the reserved
global system range. Every bootstrap mutation must carry the current lease/term
and ownership epoch, and the reserved-range write path must reject stale owners.
Routing must help clients find the owner, but correctness depends on the
reserved-range fencing gate, not on routing alone.

**Bootstrap is idempotent and compare-and-set guarded.** The owner writes
bootstrap state in the same authority domain using compare-and-set semantics:
create each missing global object only when the expected prior state is still
true, and publish the durable bootstrap-complete marker only after the required
auth, vault, config, and policy records are present. Retrying the same manifest
or preset must either observe the completed marker, roll partial state forward
idempotently, or fail because a CAS/marker check proves the state no longer
matches the intended bootstrap input. It must never fork a second global auth
state.

**Non-owner members must not bootstrap independently.** When bootstrap
credentials, presets, or a manifest are present on a non-owner, that member must
not create admins, initialize vault material, apply policy, or publish completion
locally. It may wait, forward or redirect the request to the current
reserved-range owner, or observe the completed marker once the owner commits it.

**Owner death recovers from durable reserved-range state.** If the owner dies
before completion, the next valid reserved global system range owner retries
from the durable state in that range. Partial state is handled only by the
idempotent/CAS rules above: roll partial state forward idempotently when it still
matches the expected bootstrap input, reject it when the marker or CAS checks
show a conflicting bootstrap already won, and never synthesize completion from
local node memory.

**Development no-auth cluster shape remains an explicit carveout.** Anonymous
`--no-auth` / `--dev` cluster-shaped boot remains allowed as a development mode
that skips auth/vault bootstrap. It is not the production cluster bootstrap
path, and it must not implicitly create global auth, vault, config, policy, or a
bootstrap-complete marker.

**RedDB Cloud keeps policy-first bootstrap.** RedDB Cloud keeps policy-first
bootstrap through an operator/cloud manifest. In clustered deployments the
reserved global system range owner applies that manifest as the initial global
policy source; Cloud does not get a second bootstrap authority, a system-owned
user bypass, or per-node admin creation.

## Considered Options

- **Reserved global system range owner (chosen).** Keeps global auth/vault/config
  state beside the durable completion marker and uses the same ownership,
  term/epoch fencing, retry, and recovery model as other range-owned cluster
  state.
- **Supervisor-elected owner.** Rejected as the bootstrap authority because it
  separates the decision from the data that proves completion. The Supervisor may
  elect/control ownership, but the durable bootstrap facts live in the reserved
  range.
- **Operator-selected one-shot owner.** Rejected as the default because it relies
  on out-of-band operator coordination during the most sensitive boot path. It
  may be useful as an administrative recovery command only if it still acquires
  the reserved-range ownership fence before writing.
- **Every node attempts bootstrap with local checks.** Rejected because local
  checks cannot prove that another member did not concurrently create divergent
  admins, vault material, policy state, or completion markers.

## Consequences

- Cluster auth/vault/config/policy implementation must introduce a reserved
  global system range before enabling authenticated clustered first boot.
- The existing cluster-shaped `--no-auth` / `--dev` path stays a deliberate
  development bypass while production auth bootstrap remains fenced by the
  reserved range.
- Bootstrap APIs and CLI/server env handling must route or reject non-owner
  bootstrap attempts instead of running presets locally.
- The durable marker and all bootstrap-created global records must share the
  same reserved-range commit/recovery path so restarts cannot diverge auth state.
- RedDB Cloud manifests remain ordinary policy-first bootstrap input applied by
  the reserved-range owner.
