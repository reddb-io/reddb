# Column-level policy enforcement: audit gaps + wire deny on every query path [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/240

Labels: enhancement

GitHub issue number: #240

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como autor de policy ou auditor, hoje posso escrever uma policy negando acesso a uma coluna específica:

```sql
CREATE POLICY pii_block FOR users
  DENY select ON column:users.email;
```

A policy persiste, o simulador retorna `deny`, o audit log registra a decisão. **Mas o caminho real de execução de query nem sempre aplica a policy** — Alice rodando `SELECT email FROM users` provavelmente recebe a coluna mesmo assim, dependendo do path.

`docs/security/policies.md:89-91` admite explicitamente:

> `column:*` is the canonical resource vocabulary for PII simulation and audit trails; **use safe views, RLS, generated columns, or query boundary filtering until a specific SQL path has column-level enforcement**.

E `docs/security/permissions.md:33`:

> "Can Alice read orders but not PII fields?" → Column/resource deny **where wired**, or a view/RLS workaround

"Where wired" significa: não está em todo lugar.

Isto é um gap de segurança real — operador escreve policy esperando enforcement, recebe simulador OK, mas runtime ignora. O sistema sugere proteção que não entrega. Pior do que não ter o feature: cria falso senso de segurança.

## Solution

Auditoria + wiring sistemático de column-level deny em **todos** os caminhos de leitura/escrita:

1. **Audit pass** — mapear cada query path do runtime e documentar quais aplicam column gate hoje vs. quais não. Resultado: lista exaustiva de paths e estado de enforcement.
2. **Wire em SELECT relacional** — `runtime/query_exec/table.rs` aplica column-deny sobre `extract_select_column_names`. Caminho mais frequente.
3. **Wire em document field projections** — `body->>'email'` em queries sobre documentos vira `column:logs.email` para gating; sem isso, deny escrito sobre documento é ignorado.
4. **Wire em UPDATE SET** — write-side column deny: `UPDATE users SET email = ...` é bloqueado se principal não pode escrever em `column:users.email`.
5. **Wire em INSERT** — coluna em `(col1, col2, ...)` da clause deve ser checada vs `column:` policies de write.
6. **Wire em vector/graph/timeseries paths** — descobrir quais aplicam, wire o que falta.
7. **Atualizar docs** — remover disclaimer de `policies.md` quando todas paths forem cobertas; pares `policy_simulate` ↔ `runtime` ficam alinhados.
8. **Conformance + property tests** — provar que `policy_deny ON column:X` retorna 0 rows com X em qualquer query path.

## User Stories

1. Como autor de policy, quero escrever `DENY select ON column:users.email` e ter certeza que SELECT, vector search, document projection, ou qualquer query path retorne sem `email`, para confiar na policy como enforcement real.
2. Como auditor, quero rodar uma test suite que prove que cada column deny na minha policy é honrada em runtime, para reportar compliance sem cavar implementação.
3. Como engenheiro de segurança, quero saber **antes** de escrever uma policy se o path de execução cobre column gate, para não criar falso senso.
4. Como engenheiro mantendo o runtime, quero um único módulo "column gate" reusado em cada path, para que adicionar novo SELECT path automaticamente herde enforcement.
5. Como autor de policy escrita em SQL, quero `CREATE POLICY pii FOR users DENY select ON column:users.email` que aplique em queries via Postgres-wire e RedWire e gRPC e HTTP, para enforcement uniforme.
6. Como autor de policy de write, quero `DENY update ON column:users.email` bloqueando UPDATE SET email=, para evitar que role X corrompa colunas que não pode editar.
7. Como autor de policy escrita sobre documents, quero `DENY select ON column:logs.body.password` (path JSON) bloqueando `body->>'password'` em queries, para esconder fields de documentos sem precisar criar VIEW.
8. Como engenheiro de SDK, quero que a coluna negada simplesmente apareça como `null` no resultado (não erro), para que clientes legacy não quebrem com deny ativada.
9. Como autor de release notes, quero ver no `policies.md` exatamente quais paths são cobertos, sem disclaimer "where wired", para o documento ser fonte de verdade.
10. Como autor de teste regressão, quero conformance suite com cada path × cada modelo (table/document/vector/etc) provando deny aplicada, para detectar regressão imediatamente.
11. Como engenheiro de SRE, quero log/audit trail de cada gate negado: principal X tentou ler column Y em query Z, foi bloqueado, para forensics.
12. Como auditor externo, quero comprovação automatizada (test suite passing) que policy = enforcement, para certificações de compliance (SOC2, HIPAA).

## Implementation Decisions

### Módulos novos / modificados

- **`ColumnPolicyGate` (deep module novo)** — interface única `gate(principal, action, columns: &[QualifiedColumn]) -> GateResult`. Resultado: `Allowed`, `DeniedColumns(Vec<String>)`, `Error`. Gates aplicam matching sobre patterns existentes em IAM policies (`column:table.col`, `column:*.email`, etc).

- **Modificações em runtime paths:**
  - `runtime/query_exec/table.rs` — chamar `ColumnPolicyGate::gate(principal, Select, projected_columns)` antes de retornar rows. Colunas negadas viram `null` ou são removidas (decisão por slice).
  - `runtime/query_exec/document.rs` (ou paths equivalentes) — JSON path expressões mapeadas para `column:collection.field.subfield` e gate aplicado.
  - `runtime/query_exec/vector.rs` — vector search com `RETURNING` field projection.
  - `runtime/query_exec/graph.rs` — graph traversal com property projection.
  - `runtime/query_exec/timeseries.rs` — timeseries `SELECT metric, value, tags`.
  - `runtime/dml.rs` (path INSERT/UPDATE) — write-side column gate.

- **`docs/security/policies.md`** — remover disclaimer de "where wired" quando coverage estiver completo. Adicionar tabela explícita: path × model × action × covered.

- **Conformance + integration test suite** — categorizar por (path, model, action) e provar gate em cada combinação.

### Decisões técnicas

- **Coluna negada = null** vs **coluna negada = erro 403**: na primeira slice, **null silencioso** (compat com clients legacy). Configurável via policy condition `enforce_strict: true` para erro explícito.
- **Performance**: gate aplicado uma vez por query (não por row). Project list é estática durante execução.
- **Wildcard support**: `column:*.email` matches em qualquer collection (PII pattern). `column:users.*` matches em qualquer coluna de users.
- **Document JSON paths**: tratamento especial — `body->>'email'` mapeia para `column:collection.body.email`. Implementação via path normalizer.
- **Cache**: policy decisions por (principal, action, table) cached por session/connection. Invalidação por policy change.

### API contracts

- Existing IAM policy DSL não muda — backward compat.
- Existing `policy_simulate` API não muda — só passa a refletir realidade.
- Erro retornado quando todas as colunas projetadas são negadas: `403 ColumnDenyAll`.

### Schema changes

- Nenhum schema persistido muda. Apenas runtime behavior.

## Testing Decisions

### Princípio

Testar comportamento externo: "principal P com policy X que nega `column:users.email`, faz `SELECT email FROM users`, recebe `null` no `email`." Não testar implementação interna do gate (estrutura de cache, ordem de checks).

### Cobertura nova

- **Audit relatório**: documento `docs/security/column-enforcement-coverage.md` (gerado pela slice 1) listando cada combinação (path × model × action) e estado.
- **Integration tests por path**: pelo menos 1 test por (path, model, action) provando deny respeitada.
- **Property tests**: `proptest` gerando combinações de policy + query, provando que gate matches resource pattern.
- **Performance regression**: bench provando que gate adiciona ≤5% latência em SELECT trivial.
- **Backward compat**: cluster sem nenhuma column policy não muda comportamento (zero overhead path).

### Prior art

- `runtime/impl_core.rs::rls_policy_filter()` — pattern de RLS gate existente (row-level), reusar abordagem para column.
- `extract_select_column_names()` em `runtime/query_exec/helpers.rs:233` — already extracts; só falta gate.
- IAM policy resource matching já existe em `auth_ddl.rs` — wildcard support.

## Out of Scope

- **Mudança no DSL de policy** — vocabulário `column:*` já está documentado e implementado parcialmente. Não criar nova sintaxe.
- **Cell-level encryption** — esconder valor é tema de PRD futura.
- **Dynamic data masking** (mostrar `***@***.com` em vez de email real) — fora de scope; deny é binário (null ou row).
- **PII discovery automática** — descobrir quais colunas são PII via heurística — fora.
- **Audit fields RedDB-specific** — `red_capabilities`, `red_entity_id` etc. não são column policies user-driven.
- **Cross-collection joins** com policy em uma collection que afeta outra — futuro.

## Further Notes

- Origem: gap encontrado durante grilling session 2026-05-08 sobre table metadata. Doc admite explicitamente.
- PRD pareada com #239 (table metadata) — ambas surgiram da mesma sessão.
- Estimativa: 6-9 dias. Audit pass primeiro (1-2 dias) é blocker; informa quanta wiring real há.
- Após audit, slices podem rodar em paralelo (paths são independentes).
- Não criar ADR — esta PRD não introduz decisão arquitetural nova; o vocabulário e arquitetura já existem. É puramente wiring + tests + docs.
