# Policy-scoped vault and config namespaces

Policies authorise principals against actions and **path-globbed**
resources. This applies uniformly to vault secrets (`vault:…`), config
keys (`config:…`), and tables/collections (`table:…`, `collection:…`),
so an operator can write rules like:

- `Deny vault:* on vault:red.vault/red.secret.ai.custom.*` —
  no principal, including admin, can read or write secrets in the
  `red.secret.ai.custom.*` namespace.
- `Allow config:read on config:red_config/red.config.custom.stuff.*` —
  attached to a specific user, grants visibility into one config
  sub-tree while a default-deny posture covers the rest.

This ADR records the supporting design choices that make those rules
work and stay safe.

## Actions

Canonical actions on the vault and config surfaces:

| Action                  | Gates                                                                  |
|:------------------------|:-----------------------------------------------------------------------|
| `vault:read_metadata`   | `VAULT LIST`, `VAULT GET` (metadata only — does not reveal the value). |
| `vault:read`            | Any path that returns the decrypted secret value: `VAULT UNSEAL <key>` for the latest version, the internal `resolve_vault_secret_value` (e.g. `config:read` resolving a `secret_ref`), and any future "reveal" surface. |
| `vault:unseal_history`  | `VAULT UNSEAL <key> VERSION <n>` for an older version — a strictly greater power than reading the current value, kept on its own action. |
| `vault:write`           | `VAULT PUT`, `VAULT ROTATE`, `VAULT DELETE` (write-side mutations on entries). |
| `vault:unseal`          | The master-key seal/unseal lifecycle of a vault collection (orthogonal to reading individual secrets). |
| `vault:purge`           | Hard-purges a deleted secret beyond the tombstone window. |
| `config:read`           | Reading from a `red_config` key. |
| `config:write`          | Writing/updating a `red_config` key. |
| `config:delete`         | Deleting a `red_config` key. |

`vault:read` was introduced specifically so that policies can scope
*value reveal* independently from *metadata listing* and from the
master-key seal/unseal lifecycle. A `Deny vault:read` blocks
`VAULT UNSEAL` and any `secret_ref` indirection without affecting the
operator's ability to enumerate entries with `VAULT LIST`.

## Resource shape and globbing

Resource references compile into one of three patterns:

- `Wildcard` — `*` matches every resource.
- `Exact { kind, name }` — `vault:red.vault/red.secret.api.user.alice`.
- `Glob(raw)` — `vault:red.vault/red.secret.ai.*` or
  `config:red_config/red.config.custom.stuff.*`.

Glob matching is segment-aware (`compile_glob` /
`glob_matches` in `crates/reddb-server/src/auth/policies.rs`), so a
prefix policy on `red.secret.ai.*` covers every alias the AI subsystem
will ever read for that provider family. The platform tenant prefix
(`tenant/<t>/…`) is folded in by `qualified_name`, so the same glob
works under multi-tenancy without rewriting every rule.

## Admin is not a bypass

ADR 0021 establishes that admin authority is policy-derived. The
evaluator scans every statement for `Deny` before considering any
admin fallback, so an explicit `Deny vault:read on …` is honoured even
when the caller is `Role::Admin`. The remaining `AdminBypass` shortcut
(grants when no policy matched) is being retired separately so that
default-deny postures can be expressed without anti-patterns; until
then, operators who need that posture today should attach an explicit
allow-all to the admin user and rely on per-namespace `Deny` to carve
exclusions.

## AI credential resolver — system read with audit

When a query needs an AI provider key (e.g. `INSERT … WITH AUTO EMBED
USING openai`), `crate::ai::resolve_api_key_from_runtime` reads the
secret as system: the AI subsystem must be able to obtain the key the
query needs, and policy enforcement happens at the *query* layer (who
can run AUTO EMBED at all, who can target which collection), not at
the resolver.

Every resolution emits an `ai.credential.resolve` audit event
containing the calling principal, the provider, the alias, every path
checked, and whether it hit — but never the secret value. This closes
the observability gap that "user X triggered a read of
`red.secret.ai.openai.default.api_key` via AUTO EMBED" used to be
invisible.

## Consequences

- Operators can express namespace-scoped read/write/delete restrictions
  for both vault and config paths without code changes.
- A `Deny` on a vault or config glob is the authoritative way to lock
  a namespace, including against admin (Deny scan runs before any
  admin allow consideration).
- Pre-existing policies that allowed `vault:unseal` to authorise
  reveal of the current value must migrate to `vault:read`.
  `vault:unseal_history` remains in place for cross-version reveals.
- AI credential reads remain best-effort from the query's perspective
  (the resolver does not deny based on principal); the audit trail is
  the lever for incident response and access reviews.
