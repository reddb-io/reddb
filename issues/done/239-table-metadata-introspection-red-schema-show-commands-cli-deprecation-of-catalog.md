# Table metadata & introspection: red.* schema, SHOW commands, CLI, deprecation of /catalog granular endpoints [PRD]

GitHub: https://github.com/reddb-io/reddb/issues/239

Labels: enhancement

GitHub issue number: #239

## Status

Parent/PRD/umbrella issue. Kept out of Ralph's top-level implementation queue.

## Original GitHub Body

## Problem Statement

Como operador, mantenedor ou cliente do RedDB, preciso responder perguntas básicas que hoje não tenho resposta direta:

- Quais collections existem neste cluster? Posso filtrar só as `table`?
- Qual delas está crescendo mais rápido em disco? Quanto consome de RAM?
- Qual schema tem `users`? Quais índices? Quais policies?
- Quantas linhas em cada coleção? Qual fragmentação (segmentos)?
- Qual o registro de saúde — qual coleção precisa de atenção?

Hoje a única superfície é `GET /catalog` HTTP retornando JSON gigante com 18 endpoints granulares. Não há comando SQL/RQL para descoberta. Clientes Postgres-wire (Prisma, JDBC, Metabase, DBeaver) consultam `pg_class`/`pg_namespace` no handshake e recebem resposta vazia ou erro — visualizam schema vazio. SDK humanos têm que parsear JSON.

Em multi-tenant, sem comando SQL nativo, não há controle granular do que cada principal vê — operador ops fica trancado em decisões de RBAC/RLS herdadas de pre-3.x.

Disk-space accounting por collection **não existe**: confirmei empiricamente. Memória sim, disco não. Capacity planning hoje exige comparar arquivos do filesystem com o catalog manualmente.

## Solution

Surface unificada de introspection sobre as Collections do RedDB:

- **`red.*` schema** — virtual tables (`red.collections`, `red.indices`, `red.stats`, `red.policies`, `red.columns`) que expõem o catálogo como queries SQL/RQL convencionais. Schema canônico, namespace dedicado, estabilidade contratual via ADR 0011.
- **`SHOW *` commands** — açúcar humano sobre `red.*`. `SHOW COLLECTIONS` lista tudo; `SHOW TABLES`/`SHOW QUEUES`/`SHOW VECTORS`/`SHOW DOCUMENTS`/`SHOW TIMESERIES`/`SHOW GRAPHS`/`SHOW KV` filtram por modelo. `SHOW SCHEMA <name>`, `SHOW INDICES ON <name>`, `SHOW POLICIES ON <name>`, `SHOW STATS [<name>]`, `SHOW SAMPLE <name>` para detalhes.
- **Disk-space accounting per-collection** — novo módulo `DiskAccountant` que computa bytes em disco walking o B-tree de cada `collection_root`. Cache 30s. Coluna `on_disk_bytes` em `red.collections`.
- **Internal collection flag** — DLQs, audit log, auto-policy artifacts e outras collections gerenciadas pela engine ganham `internal: true`. Default `SHOW COLLECTIONS` esconde; `SHOW COLLECTIONS INCLUDING INTERNAL` revela.
- **Postgres-wire compatibility via translator** — novo módulo `wire/postgres/translator.rs` que reescreve queries `pg_class`/`pg_namespace`/`pg_attribute`/`pg_index` para `red.*`. ORMs/BI tools (Prisma, SQLAlchemy, Hibernate, Metabase, DBeaver) conectam sem patch. Engine continua sem nenhum conhecimento de conceitos PG. Conforme ADR 0010.
- **CLI `red admin collections {list,show,stats}`** — comandos human-friendly que executam o SQL nativo internamente. `--json`/`--csv`/`--no-color` para scripts.
- **Migração gradual de `/catalog/*` HTTP** — endpoints agregados (`GET /catalog`, `/readiness`, `/attention`) ficam. Granulares (`/catalog/indexes/declared`, `/operational`, `/graph/projections/*`, `/analytics-jobs/*`) marcados como `Deprecation` em ≥1 release antes de remover. Substituídos por `POST /query` + `SELECT FROM red.indices WHERE ...`.

Tudo respeita `EffectiveScope` (tenant filter mandatório) e capability `red:catalog:read` (default true para autenticados, granular bypass para `cluster:admin`).

## User Stories

1. Como operador, quero rodar `SHOW COLLECTIONS` e ver toda Collection do meu tenant com nome, modelo, contagem de linhas, bytes em RAM e em disco, para diagnosticar uso de recursos sem sair do SQL.
2. Como operador, quero rodar `SHOW TABLES` e ver apenas Collections do tipo `table`, sem misturar queues/vectors/documents, para focar em diagnóstico relacional.
3. Como operador, quero rodar `SHOW QUEUES`/`SHOW VECTORS`/`SHOW DOCUMENTS`/`SHOW TIMESERIES`/`SHOW KV`/`SHOW GRAPHS` e ver apenas Collections do modelo correspondente, para auditoria por workload.
4. Como operador, quero rodar `SHOW COLLECTIONS WHERE on_disk_bytes > 1073741824` e identificar Collections passando 1 GiB, para capacity planning.
5. Como operador, quero rodar `SHOW SCHEMA users` e ver as colunas, types, constraints e default values, para entender estrutura sem precisar caçar no `CREATE TABLE` original.
6. Como operador, quero rodar `SHOW INDICES ON users` e ver os índices ativos, seus tipos e estado de saúde, para diagnosticar lentidão de query.
7. Como operador, quero rodar `SHOW POLICIES ON users` e ver as IAM policies + RLS predicates anexados, para auditoria de segurança.
8. Como operador, quero rodar `SHOW STATS users` e ver entities, segments, hot/cold ratio, last write timestamp, para entender atividade.
9. Como operador, quero rodar `SHOW SAMPLE users LIMIT 10` e ver as primeiras 10 linhas (não amostra aleatória), para sanity check.
10. Como operador, quero que `SHOW COLLECTIONS` por default **não** mostre DLQs internos, audit_log, auto-policy artifacts, para que minha primeira impressão do banco seja limpa.
11. Como operador, quero rodar `SHOW COLLECTIONS INCLUDING INTERNAL` e ver tudo, incluindo internal collections, para investigação operacional.
12. Como operador root com `cluster:admin`, quero ver Collections de todos os tenants em uma única query, para auditoria de plataforma.
13. Como tenant `acme`, quero **nunca** ver Collections do tenant `globex` em meu `SHOW COLLECTIONS`, mesmo se errar a configuração, porque o engine deve forçar o filtro de tenant antes de qualquer policy.
14. Como cliente Prisma/Hibernate, quero conectar via Postgres wire e ver minhas tables no autocomplete sem patch, porque o adapter PG traduz `pg_class` para `red.collections` internamente.
15. Como cliente Metabase/DBeaver, quero descobrir tables, columns, indexes via Postgres wire e enxergar tudo no UI normal, porque a tradução PG cobre `pg_class`/`pg_attribute`/`pg_index`.
16. Como autor de SDK, quero acessar metadata via SQL `SELECT FROM red.collections` em vez de parsear JSON do `GET /catalog`, para integração mais natural.
17. Como operador rodando script de capacity planning, quero `red admin collections list --json` e receber JSON estruturado com todas as colunas + on_disk_bytes, para integração com Grafana/Prometheus.
18. Como operador, quero `red admin collections show users` e ver schema + índices + policies + stats em uma página formatada, para diagnóstico rápido sem decorar SQL.
19. Como operador, quero `red admin collections stats users --json | jq '.in_memory_bytes'`, para extrair métricas isoladas em scripts.
20. Como engenheiro de capacity, quero saber quanto de disco cada coleção consome, e quero precisão de até ~5% (suficiente para tomar decisão de rebalanceamento), porque a alternativa hoje é zero info.
21. Como engenheiro de SRE, quero saber qual collection está com mais segments (sinal de fragmentação), para priorizar compaction.
22. Como engenheiro de SRE, quero saber a flag `attention` em `red.collections` e o motivo (`attention_reasons` em VERBOSE), para auto-detecção de problemas.
23. Como autor de policy, quero `CREATE POLICY ON red.collections USING (model != 'queue')` para esconder queues de auditores, para ACL row-level sobre o catálogo.
24. Como autor de policy, quero anexar policy `CREATE POLICY visible_to_acme ON users USING (tenant_id = CURRENT_TENANT())`, e ver que `SHOW COLLECTIONS` respeita o tenant, para isolamento.
25. Como autor de doc, quero que minha referência `docs/reference/red-schema.md` reflita exatamente o que o engine expõe, e que mudanças sigam stability policy do ADR 0011, para usuários poderem confiar no contrato.
26. Como autor de SDK, quero saber que a coluna `entities` em `red.collections` permanece presente entre minor releases, para meu código não quebrar com upgrade.
27. Como engineer adicionando um novo campo experimental no catalog, quero usar prefixo `_experimental_*` para indicar que pode mudar, sem comprometer stability.
28. Como cliente que já usa `GET /catalog/indexes/declared`, quero janela de deprecação ≥1 release antes que o endpoint suma, com header `Deprecation: <date>` no response, para migrar para `POST /query SELECT FROM red.indices` sem rush.
29. Como operador no bootstrap (sem nenhuma policy ainda), quero rodar `SHOW COLLECTIONS` imediatamente após criar o admin, sem precisar criar magic policy primeiro.
30. Como operador que cria uma collection nova, quero que `SHOW COLLECTIONS` me mostre a collection nova **imediatamente** após o CREATE, na mesma sessão/conexão (read-after-write strong dentro do nó).
31. Como autor de teste, quero conformance cases pinned para cada `SHOW *` form, para que mudanças no parser sem ajuste no doc/conformance corpus falhem CI.
32. Como autor de teste, quero property tests provando que `SELECT FROM red.collections WHERE name = 'foo'` é equivalente a buscar `foo` no catalog snapshot, para prevenir drift entre virtual table e fonte real.
33. Como autor de release notes, quero ADRs 0010 (wire adapter translation) e 0011 (red.* stability policy) já gravadas, para que decisões arquiteturais sejam visíveis no histórico.
34. Como engenheiro de plataforma, quero que adicionar futuro `wire/mongo/` siga o mesmo padrão de translator (Mongo `listCollections` → `SHOW COLLECTIONS` interno), para que arquitetura seja escalável.

## Implementation Decisions

### Módulos novos / modificados

- **`red.*` virtual schema (deep module)** — interface única `red_query(virtual_name, filter, projection) -> RowSet` que materializa cada virtual table consultando `catalog::snapshot_store()`. Suporta `red.collections`, `red.indices`, `red.stats`, `red.policies`, `red.columns`. Nenhuma das virtual tables tem storage físico — responde direto do catalog. Read-only do ponto de vista do usuário.

- **`DiskAccountant` (deep module novo)** — interface `bytes_on_disk_for(collection: &str) -> u64`. Walk do B-tree partindo de `collection_roots[collection]`, contagem de pages × page_size. Cache TTL 30s por entrada. Implementação parcial para v1 (só relacional/document, vector/timeseries em sub-slice se complexo).

- **`InternalCollectionRegistry` (deep module novo)** — interface `is_internal(name: &str) -> bool`. Reconhece DLQs (criadas via `WITH DLQ`), `audit_log`, auto-policy artifacts. Lista hardcoded inicial; futuro suporte a marcar via DDL (`CREATE TABLE ... INTERNAL`).

- **`wire/postgres/translator.rs` (deep module novo)** — interface `translate(query: &str) -> Option<String>`. Detecta queries que tocam `pg_class`/`pg_namespace`/`pg_attribute`/`pg_index`/`pg_constraint`/`pg_database`/`pg_tables`. Reescreve para equivalente em `red.*`. Cobre as 7 tabelas mais usadas em discovery por ORMs/BI.

- **Parser additions** — adicionar `SHOW` commands em `parser/dml.rs` ou novo `parser/show.rs`. Cada `SHOW *` desugar para `SELECT * FROM red.X [WHERE filter]`. Detalhes: `SHOW COLLECTIONS [INCLUDING INTERNAL] [WHERE ...]`, `SHOW TABLES`/`SHOW QUEUES`/etc (filtros tipados), `SHOW SCHEMA <name>`, `SHOW INDICES ON <name>`, `SHOW POLICIES ON <name>`, `SHOW STATS [<name>]`, `SHOW SAMPLE <name> [LIMIT N]`.

- **CLI additions em `red admin`** — sub-comandos `collections {list,show,stats}`, `indices list`, `policies list`. Output default: tabela ANSI colorida com paginação. Flags `--json`/`--csv`/`--no-color`. Internamente cada comando faz query SQL nativa (não duplica lógica).

- **HTTP `/catalog/*` deprecation** — granulares (`/catalog/indexes/declared`, `/operational`, `/graph/projections/*`, `/analytics-jobs/*`) recebem header `Deprecation: <release+1-date>` + log warning + apontam para `POST /query SELECT FROM red.indices`. Agregados (`GET /catalog`, `/readiness`, `/attention`) ficam.

- **`CollectionDescriptor` extensions** — adiciona `internal: bool` (default false) e `on_disk_bytes: u64` (computado lazy via DiskAccountant). Conforme ADR 0011, additive only.

### Decisões técnicas

- **Authentication baseline:** tenant filter aplicado por engine no `red.*` query path, antes de qualquer policy/IAM check. Não-vazável. `cluster:admin` bypass tenant filter.
- **Read access:** `red.*` é universalmente legível dentro do scope autenticado. Sem capability check em READ. Write/update gated por `cluster:admin`.
- **Read-after-write:** hot fields (`name`, `model`, `entities`, etc) sempre fresh. `on_disk_bytes` cache 30s. Mesmo nó: strong consistency.
- **Schema stability:** seguir ADR 0011. Coluna nova: livre. Rename/remove: 1+ release de deprecation warning.
- **Collection naming:** schema `red.*` é reservado. User não pode criar collection com prefixo `red.`.
- **Postgres translation:** começar com 7 views (`pg_class`, `pg_namespace`, `pg_tables`, `pg_attribute`, `pg_index`, `pg_constraint`, `pg_database`). Próximas slices se demand surgir.
- **CLI output:** tabela default + `--json` para scripts. CSV opcional.

### Schema de `red.collections` (versão inicial, additive-only ADR 0011)

| Coluna | Tipo | Descrição |
|---|---|---|
| `name` | text | nome da collection |
| `model` | text | table/document/queue/vector/graph/timeseries/kv |
| `schema_mode` | text | strict/flexible/schemaless |
| `entities` | bigint | contagem de linhas/registros |
| `segments` | int | número de segmentos |
| `indices` | int | índices operacionais |
| `in_memory_bytes` | bigint | RAM usado (sempre fresh) |
| `on_disk_bytes` | bigint | disco usado (cache 30s) |
| `attention` | boolean | flag — precisa intervenção |
| `internal` | boolean | flag — managed pela engine |
| `tenant_id` | text | escopo multi-tenant |

Verbose adiciona `attention_reasons[]`, `declared_indices[]`, `operational_indices[]`, `indexes_in_sync`, etc — superset do `CollectionDescriptor`.

### API contracts

- `red.*` é uma **virtual schema**. SELECT funciona; INSERT/UPDATE/DELETE retornam erro "system schema is read-only".
- `pg_catalog.*` é compat layer **só** acessível via PG-wire. Outros wires (gRPC, RedWire, HTTP) não respondem a `pg_*` queries.
- HTTP `POST /query` aceita SQL com `SHOW`/`red.*` igual.
- gRPC/RedWire reusam o mesmo executor; `SHOW COLLECTIONS` funciona em todos.

## Testing Decisions

### Princípio

Testar comportamento externo: "este SQL retorna estas linhas" / "este policy gate filtra esta linha do catálogo" / "este endpoint deprecated emite header `Deprecation`". Não testar implementação interna do `DiskAccountant` (page count exato), e sim "depois de inserir N rows de bytes T, `on_disk_bytes` aumenta em pelo menos N×T×0.95 (overhead)".

### Cobertura nova

- **Conformance corpus** (estende #229): caso pinned para cada `SHOW *` (≥10 forms novos), cada filtro tipado (≥7), cada virtual table (`red.collections`, `red.indices`, etc).
- **Integration tests** em `tests/`: round-trip `CREATE TABLE foo` → `SHOW COLLECTIONS` retorna `foo`; create policy hide → SHOW respeita.
- **`DiskAccountant` tests**: empirical — escreve N rows, mede `bytes_on_disk_for`, valida ordem de magnitude.
- **PG translator tests**: queries reais que Prisma/Metabase/DBeaver fazem (capturar via `tcpdump` ou docs oficiais), valida que tradução produz resultado não-vazio.
- **CLI tests**: snapshot testing do output formatado (`red admin collections list` em cluster fixture).
- **Stability policy tests**: schema regression — uma test que falha se coluna stable é renomeada sem warning path.
- **Bootstrap tests**: cluster vazio + `SHOW COLLECTIONS` retorna 0 rows sem erro.

### Prior art

- `tests/conformance/` (#229) — pattern de TOML cases.
- `crates/reddb-server/src/catalog.rs::*` — catalog snapshot já tem testes unitários.
- `crates/reddb-server/tests/` — integration test framework existente.
- ADR 0010 + 0011 já gravadas como referência arquitetural.

## Out of Scope

- **Column-level policy enforcement** — auditoria + wiring (PRD #238 separada).
- **MySQL wire adapter** — futuro, segue mesmo padrão de translator.
- **MongoDB wire adapter** — futuro, mesmo padrão.
- **Real-time stats streaming** — `SHOW STATS` é point-in-time. Mecanismo `subscribe`-style fica para PRD futuro.
- **Auto-vacuum / auto-compact triggers** baseados em `attention` — PRD separada.
- **Catalog history / time-travel** (`SHOW COLLECTIONS AS OF '<timestamp>'`) — fora de escopo MVP.
- **Visualizações UI** (dashboard web mostrando catálogo) — frontend separado.
- **Migration de pre-3.x clusters** sem `internal` flag — assume cluster fresh ou suporta valor default `false`.
- **Garantia de cross-node strong consistency** — read-after-write é local-node-strong, cluster eventually-consistent.
- **`SHOW SAMPLE` random sampling** — versão MVP retorna primeiras N linhas. Random sampling vira sub-slice se demand.

## Further Notes

- Origem: grilling session 2026-05-08 sobre table metadata. 17 questões resolvidas, 2 ADRs gravadas (0010, 0011), CONTEXT.md expandido com 8 termos novos.
- Sem `git rebase` durante implementação (regra global).
- Não criar ADRs adicionais por slice — todas as decisões arquiteturais cabem em 0010/0011 já gravadas.
- Após este PRD: rodar `to-issues` para fatiar em tracer-bullets. Estimativa inicial: 12-15 slices entre framework, comandos individuais, translator, CLI, deprecation, docs, testes.
- PRD paralela #238 cobre column-level policy enforcement — gap encontrado durante grilling.
- Estimativa total: 8-12 dias se feito sequencialmente; menos com paralelismo via Ralph.
- Alinhado com decisão "queremos ser muito bons em SQL/RQL" (#227) — descoberta de catálogo é parte da experiência SQL canônica.
