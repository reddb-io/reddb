# IAM Policy Coverage Matrix

Issue: #271
Date: 2026-05-08

This matrix tracks the IAM column-policy runtime coverage that should remain
green for the current relational policy surface. The validation test treats a
row as release-relevant when `relevant` is `yes`; any relevant row marked
`not_covered` is a hard failure.

| path | action | model | relevant | status | evidence |
| --- | --- | --- | --- | --- | --- |
| sql_text.table_explicit_projection | select | relational_table | yes | covered | tests/iam_policy_runtime.rs::select_column_policy_allows_safe_projection; tests/iam_policy_runtime.rs::select_column_policy_denies_explicit_column |
| sql_text.table_wildcard_projection | select | relational_table | yes | covered | tests/iam_policy_runtime.rs::select_column_policy_denies_wildcard_when_any_declared_column_denied |
| sql_text.join_projection | select | relational_join | yes | covered | tests/iam_policy_runtime.rs::select_column_policy_resolves_aliases_across_table_joins |
| sql_text.update_set_columns | update | relational_table | yes | covered | tests/iam_policy_runtime.rs::update_set_column_policy_allows_allowed_target; tests/iam_policy_runtime.rs::update_set_column_policy_blocks_denied_target_column |
| sql_text.update_multi_column_set | update | relational_table | yes | covered | tests/iam_policy_runtime.rs::update_set_column_policy_blocks_multi_column_update_when_one_target_is_denied |
| sql_text.update_tenant_scope | update | tenant_table | yes | covered | tests/iam_policy_runtime.rs::update_set_column_policy_uses_tenant_context |
| sql_text.insert_named_columns | insert | relational_table | yes | covered | tests/iam_policy_runtime.rs::insert_column_policy_allows_named_columns_with_table_allow; tests/iam_policy_runtime.rs::insert_column_policy_denies_explicit_denied_column |
| sql_text.insert_omitted_columns | insert | relational_table | yes | covered | tests/iam_policy_runtime.rs::insert_column_policy_ignores_omitted_denied_columns |
| sql_text.insert_multi_row | insert | relational_table | yes | covered | tests/iam_policy_runtime.rs::insert_column_policy_applies_to_multi_row_insert |
| sql_text.insert_tenant_autofill | insert | tenant_table | yes | covered | tests/iam_policy_runtime.rs::insert_column_policy_denies_tenant_auto_fill_target |
| vector_search.result_content | select | vector_search | no | not_relevant | docs/security/policies.md documents vector result-content policy separately from the relational column-policy gate |
| graph_path.traversal_projection | select | graph_path | no | not_relevant | docs/security/select-relational-column-policy-audit-2026-05-08.md scopes graph/path traversal out of the relational SELECT wiring |
