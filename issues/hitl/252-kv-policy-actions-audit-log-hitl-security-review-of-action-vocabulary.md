# KV — Policy actions + audit log (HITL: security review of action vocabulary) [HITL]

GitHub: https://github.com/reddb-io/reddb/issues/252

Labels: enhancement

GitHub issue number: #252

## Status

Requires human/security review. Kept out of Ralph's AFK queue.

## Original GitHub Body

## Parent

#238

## What to build

Hooks the new KV verbs into the existing IAM-style policy engine. Defines the action vocabulary (`kv:put`, `kv:get`, `kv:delete`, `kv:incr`, `kv:cas`, `kv:watch`, `kv:invalidate`) and wires every verb's runtime path through the policy resolver. Every decision lands in the audit log with the same shape used today for SELECT / INSERT — actor, target, action, decision, policy that fired, timestamp.

This slice is HITL because the action vocabulary is the durable contract — once we land `kv:put` etc, renaming them later breaks every downstream policy. Worth a security review before publication.

## Acceptance criteria

- [ ] Action vocabulary agreed with security: `kv:put`, `kv:get`, `kv:delete`, `kv:incr`, `kv:cas`, `kv:watch`, `kv:invalidate`. Documented in the policy reference.
- [ ] Policy resolver gains entries for each action. Allow / deny / condition rules apply identically to existing actions.
- [ ] Audit log writes a row for every verb invocation: `{ actor, action, resource, decision, policy_id, timestamp }`. Same shape as table-write audit.
- [ ] Sealed `red.secret.*` keys remain sealed when accessed via `GET red.secret.*` — the policy gate fires before the value reaches the wire.
- [ ] Integration test: principal with `kv:get` only can `GET` but is denied `PUT / INCR / DELETE / WATCH`. Audit log shows one allow + four denies.
- [ ] Multi-tenancy test: `Allow: kv:get WHERE key MATCHES tenant.${claim.tenant_id}.*` scopes a principal to their tenant's namespace; cross-tenant `GET` returns the same not-found shape as missing-key (no info leak).
- [ ] No regression on existing policy + audit paths.

## Blocked by

- #241
- #242
- #243
- #245
