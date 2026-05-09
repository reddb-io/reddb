# Column Enforcement Coverage

Final state after #265-#269.

Column policy enforcement uses the shared `ColumnPolicyGate`. Table or
collection access is still the coarse prerequisite: a column allow does not
replace a missing table allow. Once the coarse action/resource check allows the
operation, explicit column denies win for the projected or written fields.

## Runtime coverage

| Surface | Status | Resource examples | Notes |
|---|---|---|---|
| Relational `SELECT` projection | Complete | `table:users`, `column:users.email` | Covers explicit projections, `SELECT *` expansion, schema-qualified tables, table aliases, and join output fields. |
| Relational joins | Complete | `database:*`, `table:users`, `table:orders`, `column:users.email` | The existing join privilege still uses `database:*`; projected fields are resolved back to their source table before column checks. |
| `INSERT` target columns | Complete | `table:orders`, `column:orders.secret` | Covers explicit target lists, multi-row inserts, and implicit tenant auto-fill targets. Omitted denied columns do not block the insert. |
| `UPDATE SET` target columns | Complete | `table:accounts`, `column:accounts.secret` | Any denied target blocks the whole update, including multi-column updates. Tenant-qualified policy context is honored. |
| Document JSON-path projection | Complete | `column:docs.body.secret`, `column:docs.body.nested.secret`, `column:docs.*` | Covers nested path projection, base document column projection, and wildcard document projection. |
| Vector search result content | Complete | `table:embeddings`, `column:embeddings.content` | Applies to SQL `VECTOR SEARCH` result `content` projection. |
| Timeseries `SELECT` fields | Complete | `table:metrics`, `column:metrics.tags` | Applies to projected timeseries fields such as `tags`. |
| Graph `MATCH ... RETURN` properties | Complete | `table:graph`, `column:graph.secret` | Uses the global graph-property namespace `column:graph.<property>`. |

## Decision rules

- Table allow is required before column checks can allow a projection or write.
- Explicit column deny supersedes table, schema, database, or wildcard allows.
- `SELECT *` expands to visible source columns before the result is returned; a
  deny on any expanded column blocks the query instead of silently dropping it.
- Joins resolve aliases back to source tables before checking `column:<table>.<column>`.
- JSON document paths use `column:<table>.<json-column>.<path>`.
- Tenant-qualified policies continue to resolve through the same tenant context
  used by table policies.

## Canonical sensitive-field denies

Read-side PII deny:

```json
{
  "sid": "deny-pii-read",
  "effect": "deny",
  "actions": ["select"],
  "resources": [
    "column:*.email",
    "column:*.phone",
    "column:*.ssn",
    "column:*.tax_id",
    "column:*.date_of_birth",
    "column:*.passport_number",
    "column:*.password_hash",
    "column:*.api_key"
  ]
}
```

Document JSON-path deny:

```json
{
  "sid": "deny-profile-pii",
  "effect": "deny",
  "actions": ["select"],
  "resources": [
    "column:profiles.body.email",
    "column:profiles.body.ssn",
    "column:profiles.body.payment.card_number"
  ]
}
```

Write-side deny for protected fields:

```json
{
  "sid": "deny-sensitive-writes",
  "effect": "deny",
  "actions": ["insert", "update"],
  "resources": [
    "column:users.ssn",
    "column:accounts.password_hash",
    "column:tenant/acme/events.tenant_id"
  ]
}
```

## Still separate from column enforcement

Column enforcement does not replace model-level IAM hooks or RLS. DDL, direct
HTTP collection endpoints, queue-specific commands, graph analytics jobs, and
vector-specific resource verbs may still rely on their documented role gates or
model-specific controls. Use the [Permissioning Handbook](permissions.md) for
the broader authorization map.
