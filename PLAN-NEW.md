# PLAN-NEW — Fechar gap de features vs PostgreSQL

## Contexto

Comparação `postgresql.org/about/featurematrix` vs RedDB (auditado em 2026-04-19) identificou 11 gaps prioritários. Este plano detalha cada um: motivação, escopo, abordagem técnica, arquivos-chave, esforço estimado e verificação (cenário no `reddb-benchmark` que prova funcionalidade).

Premissas:
- Engine trabalha em `/home/cyber/Work/FF/reddb`.
- Verificação roda em `/home/cyber/Work/FF/reddb-benchmark` (adapters + scenarios).
- Cada feature deve sair com (a) implementação no engine, (b) scenario de bench correspondente, (c) passar no mini-duel.
- "MVP mínimo funcional" por item: o suficiente para o cenário de bench rodar e medir. Compatibilidade total com PG vem depois.

---

## Ordem de execução (proposta)

Razão: bugs e observabilidade primeiro (desbloqueiam medição), depois SQL (maior gap visível), depois infra (partitioning/replicação/backup), depois extensibilidade.

1. **#15** Secondary index maintenance — já bloqueia `mixed_workload_indexed`
2. **#13** EXPLAIN — sem plano visível, impossível depurar regressões
3. **#12** Indexes: partial, expression, covering (GIN/GiST/BRIN ficam fase 2)
4. **#1** SQL avançado: RETURNING, window, CTE recursiva, MERGE, LATERAL, GROUPING SETS
5. **#6** Constraints: exclusion, deferrable
6. **#7** Isolation: Repeatable Read + SSI
7. **#8** Savepoints, advisory locks, 2PC
8. **#5** Partitioning declarativo
9. **#10** PITR, incremental backup
10. **#11** FDW / extension loader
11. **#2** Views (regular, materialized, updatable)

---

## Item 15 — Secondary index maintenance pós-insert

**Problema**: índice criado com `CREATE INDEX` não recebe inserts posteriores. Bench `mixed_workload_indexed` falha com 1000/1000 queries retornando contagens erradas (post-seed inserts ausentes do filtered result).

**Evidência**: `reddb-benchmark/crates/bench-scenarios/src/mixed_workload_indexed.rs` log "stale index — post-seed inserts missing from filtered result".

**Escopo**:
- Toda insert/update/delete em coluna indexada deve propagar para todos os secondary indexes da tabela.
- Snapshot consistency: leituras via índice não podem perder linhas recém-inseridas antes do próximo checkpoint.

**Abordagem**:
- `storage/engine/btree/impl.rs`: adicionar hook `apply_to_secondary_indexes(txn, row_id, old_row, new_row)` chamado em `upsert`, `insert`, `update`, `delete`.
- `storage/schema/registry.rs`: manter lista de `IndexDef` por tabela e entregar ao engine em caminho de escrita.
- `index.rs`: `IndexKind` variants precisam método `insert_entry(key, row_id)` / `remove_entry(key, row_id)`.
- Coordenar com WAL: cada entrada de índice vai numa mesma record WAL da row (atomicidade).
- Para HNSW/Inverted (vector/fulltext) vale lazy rebuild em background; não-vetoriais (BTree/Hash/DocumentPathValue) atualizam inline.

**Arquivos**:
- `src/storage/engine/btree/impl.rs` (hot path)
- `src/storage/schema/registry.rs`
- `src/index.rs` (trait `SecondaryIndex`)
- `src/storage/wal/writer.rs` (batch record layout)

**Esforço**: 1-2 semanas (é o write path crítico).

**Verificação**:
- `make mini-duel` → scenario `mixed_workload_indexed` reddb_hybrid passa.
- Novo teste unitário em `src/storage/engine/btree/tests.rs`: criar índice com 1k rows, inserir +1k, consulta filtrada retorna 2k.

---

## Item 13 — EXPLAIN

**Problema**: sem plano de query visível. Bench só mede ops/sec — não sabe se ganho veio de plan change ou de I/O.

**Escopo MVP**:
- `EXPLAIN <query>` retorna árvore de operators com tipo, custo estimado, cardinalidade estimada.
- `EXPLAIN ANALYZE` adiciona custo real + rows reais + tempo por nó.
- Campos por nó: `op`, `table`, `index?`, `est_rows`, `est_cost`, `actual_rows?`, `actual_ms?`.

**Abordagem**:
- `src/query/planner.rs` (criar se não existir): `plan_cost()` já existe em `api.rs`, expor AST do plano.
- Novo endpoint SQL: `pg_sql::parse_explain(stmt) -> PlanNode`; formatter JSON + text.
- HTTP: `POST /query` com flag `{"explain": true}`; gRPC: campo `explain_mode` em request.

**Arquivos**:
- `src/query/planner.rs` (novo ou expandido)
- `src/api/http.rs` (endpoint)
- `src/api/grpc.rs` (proto: adicionar `ExplainMode enum {OFF, PLAN, ANALYZE}`)

**Esforço**: 4-5 dias.

**Verificação**:
- Novo scenario `reddb-benchmark/crates/bench-scenarios/src/explain_check.rs`: roda `EXPLAIN ANALYZE SELECT ... WHERE city = 'NYC' AND age > 30`, parseia resposta, valida que usa índice quando disponível.
- Smoke CLI: `red sql "EXPLAIN SELECT ..." ` imprime árvore.

---

## Item 12 — Indexes: partial, expression, covering

**Problema**: só BTree/Hash/FullText/Vector. Sem partial (`WHERE pred`), expression (`ON (lower(email))`), covering (`INCLUDE (col)`).

**Escopo MVP**:
- Partial: `CREATE INDEX ... WHERE <pred>`; só indexa linhas que satisfazem.
- Expression: `CREATE INDEX ... ON (expr(col))`; chave do índice é resultado de função.
- Covering: `CREATE INDEX ... INCLUDE (other_col)`; retorna col extra sem ir ao heap.
- GIN/GiST/BRIN: fase 2, não MVP.

**Abordagem**:
- `src/index.rs` `IndexDef`: adicionar `predicate: Option<Expr>`, `key_expr: Option<Expr>`, `included: Vec<String>`.
- SQL parser: aceitar sintaxe PG-compatível.
- Planner (#13): considerar partial (matching predicate) e covering (index-only scan).

**Arquivos**:
- `src/index.rs`
- `src/sql/parser.rs` (CREATE INDEX extension)
- `src/query/planner.rs`

**Esforço**: 1 semana.

**Verificação**:
- Novo scenario `bench-scenarios/src/index_advanced.rs` com 3 sub-tests: partial hit, expression hit, covering (index-only scan); medir ops/sec.

---

## Item 1 — SQL avançado

Subitens com complexidade muito diferente. Atacar em ordem crescente de esforço.

### 1a. RETURNING (1-2 dias)
- `INSERT/UPDATE/DELETE ... RETURNING col1, col2, *`.
- Engine já produz row modificada; basta expor no response.
- Arquivos: `src/sql/ast.rs` (nova variant), `src/storage/engine/*.rs` (coletar rows afetadas).

### 1b. Window functions (2-3 semanas)
- `OVER (PARTITION BY ... ORDER BY ... ROWS BETWEEN ...)`.
- Funções mínimas: `row_number()`, `rank()`, `dense_rank()`, `lag()`, `lead()`, `sum() OVER`.
- Implementar buffer de partição (materializa toda partição em memória pra fase 1, spill pra disco fase 2).
- Arquivos: `src/query/window.rs` (novo), `src/query/executor.rs`.

### 1c. CTE recursiva (1-2 semanas)
- `WITH RECURSIVE cte AS (base UNION ALL recursive) SELECT ... FROM cte`.
- Fixed-point iteration com worktable.
- Arquivos: `src/query/cte.rs`.

### 1d. MERGE (1 semana)
- Sintaxe PG15+: `MERGE INTO target USING source ON cond WHEN MATCHED THEN ... WHEN NOT MATCHED THEN ...`.
- Compõe UPDATE + INSERT + DELETE via single pass.

### 1e. LATERAL (3-5 dias)
- `FROM t, LATERAL (subquery que referencia t)`.
- Exige planner que replaneja subquery por row da outer.

### 1f. GROUPING SETS / CUBE / ROLLUP (1 semana)
- Extensão de `GROUP BY`. Desaçucarar pra UNION ALL de GROUP BYs.

**Esforço total item 1**: 6-10 semanas.

**Verificação**: para cada subitem, scenario dedicado em `bench-scenarios`:
- `returning_throughput.rs`
- `window_rank.rs`
- `cte_graph_traversal.rs` (tenta competir com Neo4j!)
- `merge_upsert.rs`
- `lateral_nested.rs`
- `grouping_rollup.rs`

---

## Item 6 — Constraints: exclusion, deferrable

**Escopo**:
- Exclusion: `EXCLUDE USING gist (room WITH =, during WITH &&)`. Requer GiST (fase 2) → começar com btree-based para tipo escalar.
- Deferrable: `CONSTRAINT ... DEFERRABLE INITIALLY DEFERRED`; checagem só no COMMIT.

**Abordagem**:
- Table constraint struct ganha `deferrable: bool`, `initially_deferred: bool`.
- Transaction tracker acumula pending constraint checks; drena no commit.

**Arquivos**:
- `src/storage/schema/table.rs`
- `src/storage/wal/transaction.rs` (pending checks queue)

**Esforço**: 1 semana (sem exclusion via GiST; só btree-based).

**Verificação**: scenario `constraint_deferred.rs`: transação que viola FK no meio mas conserta antes do commit; tem que passar.

---

## Item 7 — Isolation levels

**Estado atual**: só Read Committed.

**Escopo MVP**:
- Repeatable Read (snapshot isolation).
- Serializable (Serializable Snapshot Isolation — SSI à la PG).

**Abordagem**:
- MVCC já existe (WAL-based); falta snapshot timestamp tracking por txn.
- Cada txn captura `xmin` no início; leituras ignoram rows com `xmax <= xmin` de outras txns.
- SSI: adicionar detecção de ciclo rw-dependencies. PG usa predicate locks. Para MVP, implementar SI (sem detecção de serialization failure) e documentar como "snapshot isolation apenas".

**Arquivos**:
- `src/storage/wal/transaction.rs`
- `src/storage/mvcc.rs` (criar se não existir)

**Esforço**: 2-3 semanas (SI). SSI completo: +1 mês.

**Verificação**: scenario `isolation_phantoms.rs`: duas txns concorrentes com range query — RR não deve ver phantom.

---

## Item 8 — Savepoints, advisory locks, 2PC

### 8a. Savepoints (3-5 dias)
- `SAVEPOINT sp`, `ROLLBACK TO sp`, `RELEASE sp`.
- Pilha de snapshots WAL dentro da txn.

### 8b. Advisory locks (2-3 dias)
- `pg_advisory_lock(key)`, `pg_advisory_unlock(key)`.
- Hash map global (session-scoped ou txn-scoped).

### 8c. 2PC (1-2 semanas)
- `PREPARE TRANSACTION 'id'`, `COMMIT PREPARED 'id'`, `ROLLBACK PREPARED 'id'`.
- Persistir prepared txns em disk (sobreviver a restart).

**Arquivos**:
- `src/storage/wal/transaction.rs`
- `src/storage/wal/prepared.rs` (novo, para 2PC)
- `src/auth/locks.rs` (criar para advisory)

**Verificação**:
- `savepoint_rollback.rs`
- `advisory_lock_contention.rs`
- `two_phase_commit.rs` (simula coordinator + matar processo antes de commit prepared)

---

## Item 5 — Partitioning declarativo

**Escopo MVP**:
- `CREATE TABLE ... PARTITION BY HASH/RANGE/LIST (col)`.
- `CREATE TABLE p_1 PARTITION OF parent FOR VALUES WITH (MODULUS 4, REMAINDER 0)`.
- Partition pruning: planner elimina partições irrelevantes.
- Fase 2: partition-wise join, automatic creation.

**Abordagem**:
- Parent table é metadata: aponta pra N tabelas-filhas.
- Roteamento: INSERT resolve partição via hash/range/list da chave.
- Planner em #13 precisa conhecer particionamento pra podar.

**Arquivos**:
- `src/storage/schema/partition.rs` (novo)
- `src/storage/schema/registry.rs`
- `src/query/planner.rs`

**Esforço**: 2-3 semanas.

**Verificação**: scenario `partition_prune.rs` — 4 partições hash em `city`, query `WHERE city = 'NYC'` só lê 1 partição (comparar ops/sec vs não-particionado).

---

## Item 10 — PITR, incremental backup

### 10a. PITR (1-2 semanas)
- Archive WAL (já tem `wal/archiver.rs`).
- `red restore --target-time '2026-04-19 12:00:00'`: re-aplica WAL até timestamp.
- Exige: WAL tem timestamp por record (verificar) + tool de restore.

### 10b. Incremental backup (1 semana)
- Base backup (já tem via snapshot) + WAL delta desde LSN X.
- `red backup --incremental --since-lsn 12345`.

**Arquivos**:
- `src/storage/wal/archiver.rs`
- `src/bin/red-restore.rs` (novo)

**Verificação**: scenario `pitr_restore.rs` — seeda dados, faz backup, muda dados, restora PITR, valida estado no ponto-alvo.

---

## Item 11 — FDW / extensions

**Escopo MVP**:
- Extension loader: `src/modules/` já é modular; formalizar interface `Extension` trait + dynamic loading via `libloading`.
- FDW mínimo: `CREATE FOREIGN TABLE ... SERVER ... OPTIONS (...)`. MVP: só `postgres_fdw` equivalente (RedDB-to-RedDB).
- `IMPORT FOREIGN SCHEMA`.

**Arquivos**:
- `src/extensions/mod.rs` (novo)
- `src/fdw/mod.rs` (novo)

**Esforço**: 3-4 semanas (ecossistema é grande).

**Verificação**: scenario `fdw_roundtrip.rs` — query numa tabela foreign (outro RedDB) e ver pushdown via EXPLAIN.

---

## Item 2 — Views

**Escopo MVP**:
- `CREATE VIEW v AS SELECT ...` — view simples.
- `CREATE MATERIALIZED VIEW mv AS SELECT ...` — com `REFRESH MATERIALIZED VIEW [CONCURRENTLY]`.
- Updatable views: auto-updatable para single-table views sem agregação/DISTINCT.

**Abordagem**:
- View = query salva; resolver no parser (view expansion) antes do planner.
- Materialized view = tabela comum + metadata apontando pra query + timestamp de refresh.

**Arquivos**:
- `src/storage/schema/view.rs` (novo)
- `src/sql/parser.rs`

**Esforço**: 2 semanas.

**Verificação**: scenario `view_perf.rs` — compara query direta vs materialized view refresh+read.

---

## Cronograma agregado (otimista, 1 dev full-time)

| Fase | Itens | Semanas |
|---|---|---|
| Fase 1: observabilidade + bug fix | 15, 13 | 2-3 |
| Fase 2: SQL core | 12, 1a, 1b, 1c | 8-10 |
| Fase 3: transacional | 6, 7, 8 | 6-8 |
| Fase 4: SQL avançado | 1d, 1e, 1f | 4 |
| Fase 5: infra | 5, 10 | 5-6 |
| Fase 6: ecossistema | 11, 2 | 6-7 |

**Total**: ~6-9 meses full-time. Paralelizável com 2-3 devs para ~3-4 meses.

---

## Gates e riscos

- **Gate após Fase 1**: mini-duel com 28/28 consistente em 5 runs. Só aí começa #1.
- **Gate após cada feature**: scenario dedicado + passa no `make check-baseline` (sem regressão >10% em outras métricas).
- **Riscos**:
  - Window/CTE recursiva tocam executor inteiro — risco de regressão em queries simples. Mitigação: feature flag por query.
  - SSI completo pode inviabilizar throughput atual de writes (predicate locks). MVP SI sem SSI é ponto de pausa.
  - FDW abre superfície de segurança nova — exige review de auth.

---

## Próximos passos concretos

1. Criar branch `feature/secondary-index-maint` — atacar #15.
2. Em paralelo, desenhar protobuf de EXPLAIN (`#13`) para alinhar com bench adapter.
3. Locar baseline atual (`make lock-baselines`) antes de começar, para detectar regressões.
4. Abrir issues no repo reddb uma por item com link para este PLAN-NEW.md.
