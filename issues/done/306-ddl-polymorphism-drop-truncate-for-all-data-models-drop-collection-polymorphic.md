# DDL polymorphism: DROP/TRUNCATE for all data models + DROP COLLECTION polymorphic [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/306

Labels: enhancement

GitHub issue number: #306

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como operador ou autor de script, hoje a DDL de **remover** ou **esvaziar** Collections do RedDB é fragmentada e incompleta:

**DROP variants existentes:**
- `DROP TABLE` ✅
- `DROP QUEUE` ✅
- `DROP TIMESERIES` ✅
- `DROP GRAPH` ❌ não existe
- `DROP VECTOR` ❌ não existe
- `DROP DOCUMENT` ❌ não existe
- `DROP KV` ❌ não existe
- `DROP COLLECTION` ❌ não existe (polymorphic)

**TRUNCATE:**
- `TRUNCATE TABLE` ❌ não existe
- `TRUNCATE QUEUE` ❌ (existe `QUEUE PURGE` como equivalente)
- `TRUNCATE GRAPH/VECTOR/DOCUMENT/TIMESERIES/KV` ❌ não existem
- `TRUNCATE` aparece **só** como privilege name em GRANT (`GRANT TRUNCATE ON ...`) — não como comando real.

Consequências práticas:
- **Como operador removo um Vector ou Graph collection?** Não há DDL. Provavelmente HTTP DELETE em `/collections/<name>` ou file-system manual.
- **Como esvaziamento rápido de table?** Hoje só `DELETE FROM users` (N eventos CDC, lento, gera N delete events em event-enabled collections — slow).
- **Vocabulário inconsistente:** glossário trata Collection como conceito raiz, mas DDL não reflete — só tipos específicos têm DROP.
- **Scripts de admin/teardown** precisam saber o tipo de cada collection antes de chamar DDL correto. Quebra "polymorphic by Collection" promise do CONTEXT.md.
- **TRUNCATE como nome de privilege em GRANT sem comando real** é um sinal claro de feature incompleta.

## Solution

DDL polimórfica + cobertura completa por model:

```sql
-- Polymorphic (operador admin não precisa saber o tipo)
DROP COLLECTION users;
TRUNCATE COLLECTION users;

-- Typed (safety: rejeita se model não bate)
DROP TABLE users;            -- existe, mantém
DROP GRAPH identity;         -- novo
DROP VECTOR notes;           -- novo
DROP DOCUMENT logs;          -- novo
DROP KV settings;            -- novo
DROP TIMESERIES metrics;     -- existe, mantém

TRUNCATE TABLE users;        -- novo
TRUNCATE GRAPH identity;     -- novo
TRUNCATE VECTOR notes;       -- novo
TRUNCATE DOCUMENT logs;      -- novo
TRUNCATE TIMESERIES metrics; -- novo
TRUNCATE KV settings;        -- novo
TRUNCATE QUEUE tasks;        -- alias para `QUEUE PURGE` existente
```

Princípios de design:

1. **Polymorphic = conveniência (admin scripts)**: `DROP COLLECTION foo` resolve o model em runtime, dispatching para o handler typed correto. Não exige user saber o tipo.
2. **Typed = safety (DDL produção)**: `DROP TABLE foo` falha se `foo` é queue. Operador de produção declara intent → engine valida.
3. **`IF EXISTS` em ambos**: `DROP TABLE IF EXISTS foo`, `DROP COLLECTION IF EXISTS foo`.
4. **Event integration (#284 slice 4)**: TRUNCATE emite 1 evento `truncate` (não N delete); DROP emite 1 `collection_dropped` + queue de eventos preservada para drenagem.
5. **`TRUNCATE QUEUE = QUEUE PURGE`**: alias canônico — `QUEUE PURGE` continua funcionando para compat.
6. **Auth**: DROP requer `drop` privilege; TRUNCATE requer `truncate` privilege (já existe como GRANT name; agora ganha enforcement real).

## User Stories

1. Como operador admin teardown, quero `DROP COLLECTION users` que funcione independente do model, para scripts não precisarem case-by-case.
2. Como operador de produção, quero `DROP TABLE users` falhar se `users` for queue (model mismatch), para evitar engano por nome igual.
3. Como autor de script de cleanup, quero `DROP COLLECTION IF EXISTS foo` que não erre quando collection já não existe, para idempotência.
4. Como operador de cleanup, quero `TRUNCATE TABLE users` para esvaziar rapidamente (1 evento truncate, não N), em vez de `DELETE FROM users`.
5. Como operador de queue, quero `TRUNCATE QUEUE tasks` como alias canônico de `QUEUE PURGE tasks`, para vocabulário consistente.
6. Como operador, quero `DROP GRAPH identity` para remover collection de tipo graph, com mesma semântica de DROP TABLE.
7. Como operador, quero `DROP VECTOR notes` para vectors.
8. Como operador, quero `DROP DOCUMENT logs` para documents.
9. Como operador, quero `DROP KV settings` para kv.
10. Como operador, quero `TRUNCATE GRAPH/VECTOR/DOCUMENT/TIMESERIES/KV` para esvaziar qualquer model.
11. Como autor de event subscription (#284), quero que TRUNCATE emita 1 evento `truncate` (não N delete events) para downstream lidar com 1 op em vez de processar 1M.
12. Como autor de event subscription, quero que DROP emita 1 evento `collection_dropped` e mantenha a queue de eventos viva para consumer drenar pendentes antes de queue ser deletada manualmente.
13. Como autor de policy, quero `GRANT DROP ON collection:foo` enforcement real (operações fail sem grant).
14. Como autor de policy, quero `GRANT TRUNCATE ON collection:foo` enforcement real (já existe vocabulário em GRANT, mas sem enforcement; agora ganha).
15. Como autor de doc, quero `docs/query/ddl.md` ou similar listando todos drop/truncate forms com exemplos.
16. Como autor de SDK, quero descobrir via SQL "qual o model de `foo`?" com `SELECT model FROM red.collections WHERE name = 'foo'` (já existe via #239) — pra fazer typed DDL no SDK.
17. Como autor de teste, quero conformance corpus com cada DDL form pinned, para detectar regressão de parse.
18. Como engenheiro de SRE, quero que DROP polymorphic emita audit log com qual typed DDL foi disparado internamente, para forensics.
19. Como autor de CLI `red admin`, quero `red admin collections drop <name>` que use o polymorphic DROP COLLECTION internamente, para tooling consistent.
20. Como autor de migration, quero garantia que `DROP TABLE` continua existindo com mesma semantica (backward compat), para scripts existentes não quebrarem.

## Implementation Decisions

### Módulos / interfaces

- **Parser** (`storage/query/parser/ddl.rs` + outros DDLs):
  - Adiciona `parse_drop_<model>_body` para graph/vector/document/kv.
  - Adiciona `parse_drop_collection_body` (polymorphic).
  - Adiciona `parse_truncate_<model>_body` para todos os 7 models + collection (polymorphic).
  - Reusa pattern de `parse_drop_table_body` existente.
- **AST core** (`storage/query/core.rs`):
  - `QueryExpr::DropGraph(DropGraphQuery { name, if_exists })`.
  - `QueryExpr::DropVector(DropVectorQuery { name, if_exists })`.
  - `QueryExpr::DropDocument(DropDocumentQuery { name, if_exists })`.
  - `QueryExpr::DropKv(DropKvQuery { name, if_exists })`.
  - `QueryExpr::DropCollection(DropCollectionQuery { name, if_exists })` — polymorphic.
  - `QueryExpr::Truncate(TruncateQuery { name, model: Option<CollectionModel>, if_exists })` — model None = polymorphic.
- **Polymorphic resolver** (deep module novo `runtime/ddl/polymorphic_resolver.rs`):
  - Interface `resolve(name: &str, scope: &EffectiveScope) -> Result<CollectionModel>`.
  - Lookup catalog snapshot, retorna model.
  - Erro "collection not found" se ausente; se polymorphic chamou e collection não tem model → 404.
- **Executor** (`runtime/impl_ddl.rs` ou similar):
  - DROP COLLECTION: resolve model → dispatch para handler typed.
  - DROP TABLE/GRAPH/etc: valida model match (via resolver) → executa typed handler. Se mismatch → erro "expected table, got queue".
  - TRUNCATE: similar; TRUNCATE QUEUE delega para `QUEUE PURGE` impl existente.
- **Event integration** (com #284 slice 4):
  - DROP emite 1 `collection_dropped` event ANTES de desconectar subscription. Subscription removida do catalog. Queue preservada com lifetime mode "drained-only" (consumer pode ler pendentes; não recebe novos pushes).
  - TRUNCATE emite 1 `truncate` event com `entities_count`.
- **Auth**:
  - `drop` action em IAM policy gates DROP variants.
  - `truncate` action gates TRUNCATE variants. Vocabulary já existe em GRANT.
  - Polymorphic DROP requer `drop` privilege na collection target (não em "todos" — resolve primeiro).

### Backward compat

- `DROP TABLE`, `DROP QUEUE`, `DROP TIMESERIES` — semantica preservada.
- `QUEUE PURGE` — preservado, alias para `TRUNCATE QUEUE`.

### Integration points

- **PRD #239 (table metadata)**: `red.collections` é fonte de verdade para o resolver polymorphic. Slice 1 (#244) é blocker — sem `red.collections`, polymorphic não tem fonte authoritative.
- **PRD #284 (event subscriptions)**: slice 4 (#302) descreve TRUNCATE/DROP single-event semantics. Esta PRD implementa o lado DDL; #302 implementa o lado event.

### Auth

- DDL requer `drop` ou `truncate` privilege na collection.
- IAM policy resource: `collection:<name>` ou `collection:<schema>.*`.
- `GRANT DROP, TRUNCATE ON collection:public.* TO admin_role` agora vira meaningful policy.

## Testing Decisions

### Princípio

Testar comportamento externo: "DROP TABLE foo remove collection com model=table; falha em queue" / "DROP COLLECTION foo dispatcha para handler correto" / "TRUNCATE QUEUE = QUEUE PURGE no que diz respeito à observação externa".

### Cobertura

- **Conformance corpus**: 1+ caso pinned para cada DDL form (DROP × 8, TRUNCATE × 7+collection). ≥15 casos novos.
- **Integration tests**:
  - DROP TABLE em queue → erro "model mismatch".
  - DROP COLLECTION polymorphic → resolve correto.
  - TRUNCATE TABLE → rows zeradas, schema preservado.
  - TRUNCATE QUEUE = QUEUE PURGE produz mesmo estado.
  - DROP em collection com event subscription → 1 `collection_dropped` event + queue drenada.
  - TRUNCATE em collection com event subscription → 1 `truncate` event.
- **Auth tests**: principal sem `drop` privilege → 403; com → success.
- **IF EXISTS**: drop de collection inexistente sem IF EXISTS → erro; com IF EXISTS → success silently.

### Prior art

- `DROP TABLE` existing tests in `parser/tests.rs`.
- `DROP TIMESERIES` parsing pattern.
- `QUEUE PURGE` existing impl.
- Conformance corpus framework (#229).

## Out of Scope

- **DROP DATABASE / DROP SCHEMA** — RedDB não tem esses conceitos hoje (multi-tenant via EffectiveScope, não schemas Postgres-style).
- **CASCADE / RESTRICT** options — RedDB não tem foreign keys em modelo unified; CASCADE é redundante. Adicionar futuro se cross-collection refs requererem.
- **TRUNCATE com RESTART IDENTITY** — sequences fora de scope (#X futuro).
- **Bulk DROP/TRUNCATE** (`DROP COLLECTIONS WHERE pattern`) — fora.
- **Soft delete / undo** — DROP é destructive; recovery via backup/restore.
- **DROP/TRUNCATE em red.* virtual tables** — sempre rejeita (system schema is read-only).

## Further Notes

- Origem: pergunta de user em 2026-05-08 sobre DROP/TRUNCATE/COLLECTION coverage.
- Auditoria em código confirmou: 3/7 DROP existem; TRUNCATE só como GRANT name; PURGE só pra queue.
- Sem ADR — decisão segue padrões SQL clássicos com adição polymorphic. Não é trade-off único.
- Pareada com #239 (red.collections — bloqueia polymorphic resolver) e #284 (event integration on TRUNCATE/DROP).
- Estimativa: 4-6 dias.
- Após `to-issues`: ~6-8 slices.
- Sem `git rebase` durante implementação (regra global).
