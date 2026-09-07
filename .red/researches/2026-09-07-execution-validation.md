# RedDB: custo de unicidade e execução vetorial verificável

Data: 2026-09-07. Continuação de #2270, sobre a base do PR #2272
(`796acb2dd4b97a62292062d62dae12d10db0e06a`).

## Resultado desta etapa

A validação de unicidade deixava o custo crescer com a tabela porque clonava
cada entidade, inclusive seu payload, e materializava mapas de todos os campos
antes de comparar a chave. O caminho agora empresta as entidades e compara
somente os campos das restrições. Mantém a representação de igualdade anterior,
a ordem das regras, NULL, exclusão do próprio registro no UPDATE e ON CONFLICT.
Ainda existe uma varredura O(N) por restrição; esta etapa não implementa um
índice transacional de unicidade nem muda o WAL, fsync ou isolamento.

O plano vetorial agora distingue `vector_turbo_search` de `vector_exact_scan`
a partir do contrato usado pelo executor. Um HNSW/IVF declarado no catálogo
não transforma a rota runtime em ANN. O plano informa explicitamente quando
esses índices não são utilizados. O plano simples consulta o marcador/contrato,
sem materializar estado TurboQuant ou disparar reconstrução.

`EXPLAIN ANALYZE VECTOR SEARCH ...` executa a consulta pelo frame tipado,
incluindo autorização, e retorna contadores reais de uma execução nova.
O EXPLAIN ANALYZE de DML mantém o rollback de ADR 0071.

## Evidência de desempenho

Medições diagnósticas, em host compartilhado, sem compilação nossa concorrente.
Valores em microssegundos por INSERT: mediana da média de cinco execuções,
após um aquecimento, com o payload de #2270 e um escritor sequencial.
Todas as chaves e payloads foram comparados antes e depois de reabrir cada
banco. SQLite usa WAL e synchronous=FULL; RedDB mantém suas opções duráveis.

| Armazenamento | Linhas | Antes | Depois | SQLite FULL | Variação RedDB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Disco | 200 | 1970,84 | 1870,01 | 970,21 | -5,1% |
| Disco | 1000 | 2656,42 | 2476,87 | 842,67 | -6,8% |
| tmpfs | 200 | 376,51 | 303,55 | 17,11 | -19,4% |
| tmpfs | 1000 | 1013,58 | 502,31 | 14,41 | -50,4% |

Os blocos SDK foram executados antes/depois, sem randomização. Houve um
outlier de 9233 us no bloco disco/1000/antes; as cinco execuções e os hashes
estão na evidência. Cinco repetições não estabelecem um ganho estável de 7%
em disco. RedDB usa o SDK Bun publicado via subprocesso stdio e SQLite roda
no próprio processo. Essa tabela mede o percurso de aplicação, não só o motor.
FULL aproxima a intenção de durabilidade, sem provar equivalência completa;
tmpfs remove a latência do dispositivo, mas não persiste após perda de energia.

Não comparar estes valores diretamente com os horários anteriores do relatório
[foundations](2026-09-07-foundations-validation.md): a carga do host variou.
A comparação relevante usa os binários antes/depois deste mesmo experimento.
Os binários medidos precedem apenas os ajustes finais de instrumentação e
planejamento vetorial; o código de unicidade é o mesmo. Seus hashes e o hash
do arquivo de unicidade identificam precisamente o que foi medido.

### Isolamento do motor nativo

Seis pares antes/depois, alternando a ordem a cada repetição, 1.000 INSERTs
por banco explicitamente em `/dev/shm`. O probe liga diretamente a biblioteca
Rust, sem SDK/subprocesso, e verifica cada chave e payload na sessão.

| Schema | Antes | Depois | Últimas 100 antes | Últimas 100 depois |
| --- | ---: | ---: | ---: | ---: |
| Com PRIMARY KEY | 670,18 | 317,58 | 1208,30 | 497,92 |
| Sem chave (controle experimental) | 201,48 | 204,48 | 253,05 | 254,36 |

O controle sem chave quase inalterado e a queda de 52,6% com chave sustentam
o diagnóstico, sem recomendar remover restrições. A curva ainda cresce:
a otimização evita cópias, mas não remove a busca linear.

A amostragem GDB do processo filho nativo encontrou a validação de unicidade
em 40 de 120 stacks. É evidência de localização de trabalho, não estimativa
precisa de porcentagem de CPU. Tempos instrumentados não entram nas tabelas.
`perf` não estava autorizado pela configuração do host; ela não foi alterada.
A API `RedDBOptions::in_memory()` hoje cria armazenamento efêmero em arquivo;
por isso o probe usa `/dev/shm` explicitamente e não anuncia um motor sem I/O.

### Esboço dos recursos

Antes: para N escritas com chave e payload P, aproximadamente
N(N-1)/2 visitas e cópias de payload/mapas; depois: o mesmo número de visitas,
com assinaturas apenas dos campos da chave e sem buffer de entidades clonadas.
Rede/stdio: mesmo número de chamadas. Disco: mesma ordem de persistência e
barreiras. CPU: ainda O(N²) acumulado, com menos alocação/cópia. Memória:
remove o buffer transitório proporcional ao tamanho da tabela; mantém locks
de leitura durante as comparações, cujo efeito sob contenção ainda precisa
ser medido. Não foi medido pico de RSS do servidor nesta etapa.

## Contrato dos contadores vetoriais

Exemplo:

```sql
EXPLAIN ANALYZE VECTOR SEARCH places SIMILAR TO [1.0,0.0]
WHERE category = 'yes' LIMIT 1
```

A resposta contém uma linha com `op`, `source`, `index_used`,
`candidates_examined`, `metadata_rejected`, `visibility_rejected`,
`exact_distance_evaluations`, `actual_rows`, `actual_ms` e
`metrics_scope = vector_pipeline`. O tempo cobre o pipeline vetorial,
incluindo seu planejamento; não inclui todo o frame/autorização externo.
Não são métricas individuais inferidas para cada nó do plano lógico.

- `candidates_examined`: entidades já visíveis retornadas pelo scan legado,
  ou hits produzidos pelo TurboQuant que o pipeline visita. Não mede os
  cálculos aproximados internos do índice nem todos os tombstones físicos.
- `metadata_rejected`: candidatos descartados pelo predicado efetivo.
- `visibility_rejected`: hits TurboQuant ausentes ou invisíveis no snapshot.
  No scan exato, a visibilidade já foi aplicada pelo manager antes da contagem.
- `exact_distance_evaluations`: chamadas reais de distância exata, incluindo
  reranking TurboQuant. No caminho legado conta após a deduplicação do índice.
- `actual_rows`: linhas após filtros, threshold e top-k.
- Consultas vetoriais comuns também expõem `stats.vector`. `cache_hit=true`
  identifica medidas da computação armazenada em cache, não trabalho novo.
  Medidas vetoriais não são recuperadas como execução a partir do cache
  persistido; seu formato antigo continua decodificável. O backend blob
  mantém o fallback de fingerprint não decodificável já existente.
  O ANALYZE sempre executa novamente.

`CREATE VECTOR` já cria coleções TurboQuant desde #675. Coleções legadas sem
esse marcador usam o scan exato. O relatório anterior capturou o rótulo antigo
`vector_exact_scan` para uma coleção nova: isso era o plano anunciado, não
prova de qual algoritmo tinha executado. Esta mudança corrige a discrepância.

A resposta JSON ganha um campo opcional. Na API Rust, literais exaustivos de
`QueryStats` precisam incluir o novo campo `vector` ou usar `..Default::default()`.
Os nomes dos operadores vetoriais do plano mudam intencionalmente.

## Verificação

No binário final do commit `0f773cd840e1b0639755e3042b9cd4ee13025b80`:

- `cargo check --locked --tests -p reddb-io`, formatação e docs matrix passaram.
- Duas regressões CLI de métricas vetoriais, quatro regressões RPC de unicidade
  e cinco de dump/restore passaram.
- Três programas Bun passaram: SDK, fallback legado e cliente RedWire real.
- Cinco casos Rust passaram como smoke ligado à biblioteca release, gerado
  do mesmo fonte registrado no harness `grouped_ai_search`: cache/fresh ANALYZE,
  TurboQuant, HNSW declarado na rota legada, negação IAM e transação preservada.
  Esse smoke não é uma execução do harness Cargo; os checks da PR registram
  a execução adicional desse harness.
- [Dez casos do oráculo RedDB/SurrealDB](evidence/2026-09-07/execution-multimodel-quality.json)
  passaram, incluindo vetor fracionário e composição vetor + geo + metadados.
- [Recuperação após SIGKILL](evidence/2026-09-07/execution-recovery.json):
  todas as 200 chaves e payloads reconhecidos foram verificados; o dump/restore
  em um banco novo preservou os mesmos valores. Não é simulação de perda de energia.

[Hashes e logs de validação](evidence/2026-09-07/execution-validation.json).
Os testes novos integram o harness `grouped_ai_search`; os scripts CLI/RPC
entram no job Driver Runnable Examples, que o workflow atual executa apenas
com `workflow_dispatch full_ci=true`.

Reprodução local:

```sh
cargo check --locked --tests -p reddb-io
cargo test --locked --test grouped_ai_search e2e_vector_execution_observability -- --test-threads=1
REDDB_BINARY_PATH=/absolute/path/to/red python3 tests/cli_vector_explain.py
REDDB_BINARY_PATH=/absolute/path/to/red python3 tests/stdio_uniqueness.py
```

Dados: [execuções, hashes e resumo](evidence/2026-09-07/execution-performance.json),
[stacks nativos](evidence/2026-09-07/execution-native-stack-samples.json) e
[probe nativo](evidence/2026-09-07/execution-native-probe.rs). As amostras
individuais permanecem em `/tmp/reddb-competitive-audit/results/execution`;
os diretórios dos bancos de benchmark foram removidos após a verificação.
O runner SDK reproduzível é `competitive/audit.mjs` no benchmark PR #2.

## Qualidade contra SurrealDB e próximos limites

Aprendemos com a observabilidade do SurrealDB: medir execução e explicar
por que um índice não foi usado. RedDB ganha essa capacidade no pipeline
vetorial, ainda sem a granularidade por operador/batch observada no SurrealDB.
Isso melhora a capacidade de diagnosticar e escolher planos; não melhora
sozinho relevância, recall, qualidade de embeddings ou isolamento.

A filtragem TurboQuant ainda pode pedir até o tamanho da coleção e reranquear
os elegíveis. A busca exata ainda reconstrói BruteForceVectorIndex por consulta,
cujo upsert linear pode ser quadrático. Precisamos medir corpus grande,
seletividade rara, memória e recall antes de escolher expansão adaptativa.
A próxima campanha deve juntar documento + grafo + vetor + geo, updates/deletes,
empates, valores ausentes e RLS. O gate de permissão de coleção testado aqui
não certifica ausência de vazamento por contagem/ordenação sob RLS por registro.

Durante a preparação das fixtures apareceram limitações já presentes na base:
PK composta com sintaxe PRIMARY KEY(a,b) não é aceita; UNIQUE parece não ser
recuperada no percurso CLI entre processos; reutilizar PK após DELETE também
falhou na base. Estes achados não foram resolvidos nesta otimização e pedem
reproduções próprias de persistência/semântica antes de ampliar a alegação de
correção. #2270 e o programa competitivo completo permanecem abertos.
