# Parser coverage uplift C: lexer.rs (46→90%) + table.rs (49→90%) [AFK]

## Parent

#227

## What to build

Subir cobertura de:

- `lexer.rs`: 46.03% → ≥90%. Cobrir: todos os tokens (todas keywords), escapes em strings (`\n`, `\t`, `\\`, `\'`, `\"`), strings com aspas duplas, números (int, float, negative, scientific notation, hex/binary se suportado), comentários `--` e `/* */`, JSON sub-mode (balanced/unbalanced/empty), depth limits, todas as variantes de operadores (`<>`, `!=`, `>=`, `<=`, `||`, `->`, `->>`).
- `parser/table.rs`: 48.92% → ≥90%. Cobrir: SELECT com todas projeções (`*`, alias, function, arithmetic, subquery), FROM com schema-qualified names, JOIN (INNER/LEFT/RIGHT/FULL/CROSS/LATERAL), WHERE com todas comparações + IN/NOT IN/BETWEEN/IS NULL/EXISTS, GROUP BY, HAVING, ORDER BY (ASC/DESC, nulls first/last), LIMIT/OFFSET, FROM ANY, subqueries em FROM.

## Acceptance criteria

- [ ] `lexer.rs` ≥90% lines.
- [ ] `table.rs` ≥90% lines.
- [ ] Cada keyword definida no `Token` enum exercitada por ≥1 teste de parsing.
- [ ] Workflow #228 reporta os novos números no PR.
- [ ] Sem regressão em `cargo test -p reddb-server`.

## Blocked by

- #228
- #229
