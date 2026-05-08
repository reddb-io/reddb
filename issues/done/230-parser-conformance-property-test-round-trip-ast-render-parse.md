# Parser conformance: property test round-trip AST→render→parse [AFK]

## Parent

#227

## What to build

Property test usando `proptest` (já em dev-deps) que prova: para um subset de `QueryExpr`, `parse(render(ast)) == ast`. Round-trip identity.

Inclui:
- `AstRenderer` parcial novo: trait/impl que renderiza `QueryExpr` de volta para SQL/RQL canônico. Cobre apenas o subset que o property gerador produz nesta slice (SELECT projection FROM table WHERE filter; INSERT INTO table (cols) VALUES; QUEUE PUSH q value).
- `proptest` strategy gerando ASTs válidas dentro do subset.
- Test que executa 256 casos (default proptest) e shrinks contra-exemplos.

`AstRenderer` é módulo profundo: interface única `render(&QueryExpr) -> String`. Pode ser estendido em PRDs futuros.

## Acceptance criteria

- [ ] `AstRenderer` em `src/storage/query/renderer.rs` (ou similar) cobre subset do property gerador.
- [ ] Property test em `parser/property_tests.rs` ou estende `parser/tests.rs`.
- [ ] Property gera ≥3 categorias de query (SELECT, INSERT, QUEUE PUSH com JSON).
- [ ] Round-trip passa em 256 casos default.
- [ ] Contra-exemplos encontrados (se houver) viram casos fixos no conformance corpus.
- [ ] Documentado: como rodar property test isolado (`cargo test -p reddb-server property_round_trip`).

## Blocked by

None - can start immediately
