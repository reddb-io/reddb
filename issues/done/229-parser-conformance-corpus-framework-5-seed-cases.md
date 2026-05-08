# Parser conformance: corpus framework + 5 seed cases [AFK]

## Parent

#227

## What to build

Estabelecer estrutura de teste reutilizável para conformance + migrar os 8 testes `doc_form_*` (commit 219b0c42) como seeds.

Estrutura proposta (decidida na primeira slice):
- `crates/reddb-server/tests/conformance/` — diretório novo.
- Casos como arquivos `.toml` ou `.sql` + `.expected.toml` pareados.
- Runner único `tests/conformance.rs` que itera o diretório, parseia cada `input`, valida `expected`.
- `ConformanceCase` struct com: `input`, `expected_kind` (variante de `QueryExpr`), `source` (file:line de doc), `kind` (positive | negative).
- Helper `assert_parses_as!(input, QueryExpr::Insert)` ou similar para minimizar boilerplate.

## Acceptance criteria

- [ ] `tests/conformance/` criado com runner em `tests/conformance.rs`.
- [ ] 5 seed cases positivos rodando: 1× QUEUE PUSH com JSON literal, 1× INSERT DOCUMENT, 1× INSERT timeseries com tags JSON, 1× SELECT simples, 1× CREATE TABLE simples.
- [ ] Cada caso aponta para `source` no arquivo de doc original (file:line).
- [ ] `cargo test -p reddb-server --test conformance` passa.
- [ ] README curto em `tests/conformance/README.md` explica como adicionar caso novo (cópia + edição, sem código).
- [ ] Os 8 testes `doc_form_*` em `parser/tests.rs` permanecem (não duplicar nem remover) — o corpus expande, não substitui.

## Blocked by

None - can start immediately
