# Qualidade multimodelo: RedDB e SurrealDB

Data: 2026-09-07. Complemento à auditoria #2271 e às correções #2272.

## Julgamento

Nas frentes examinadas, o SurrealDB demonstra maior integração entre linguagem,
execução e diagnóstico de consultas compostas. O RedDB tem fundamentos úteis
para competir — entidade/coleção comum, planos canônicos, frame de leitura com
snapshot e escopo, busca de contexto e ASK com fontes — mas a existência dessas
peças não prova que todas as combinações tenham a mesma qualidade.

O teste novo de vetor + geografia + metadados retornou respostas corretas nos
dois motores. A diferença mais clara nessa amostra não foi o resultado, mas a
explicação da execução e a confiabilidade do caminho público do SDK. O RedDB
rejeitou um parâmetro `Float32Array([0.8, 0.6])` que os testes com `[1, 0]` não
cobriam. A correção desse decoder foi incluída em #2272.

Não há evidência suficiente para declarar um vencedor geral de qualidade,
isolamento, segurança ou relevância. Também não há fundamento para concluir
que adicionar mais comandos de IA tornaria o banco melhor. A oportunidade é
fazer as capacidades existentes comporem respostas corretas, com planos
eficientes, garantias consistentes e diagnóstico verificável.

## Escopo e evidência

- Execução RedDB: binário otimizado da base `85153296d8beba3bfa5ded2dfa719f416f50393d`,
  SDK publicado `1.23.2`, Bun `1.4.1`. O resultado anterior às correções está em
  [multimodel-quality-before.json](evidence/2026-09-07/multimodel-quality-before.json).
- Execução RedDB corrigida (#2272): os dez casos passaram, incluindo o vetor
  fracionário via SDK. [Resultado posterior](evidence/2026-09-07/multimodel-quality-after.json).
- Execução SurrealDB: servidor `3.2.4`, RocksDB, SDK JavaScript `2.0.8`.
- Fonte SurrealDB: revisão `93ab219d69f09d8f999851b0359c80ebe6726102`. Uma leitura
  dessa revisão não prova que cada otimização esteja na distribuição `3.2.4`.
- Probe reproduzível: `src/runners/bun/competitive/quality.mjs` no PR
  [reddb-benchmark#2](https://github.com/reddb-io/reddb-benchmark/pull/2).
- Os tempos internos retornados por EXPLAIN não são uma comparação de velocidade:
  a máquina é compartilhada e havia compilação concorrente.

### Consulta composta executada

Cinco documentos lógicos carregam vetor, categoria e localização. Os maiores
scores vetoriais pertencem a candidatos de outra cidade, outra categoria ou
sem localização. Os únicos elegíveis são `near_a` e `near_b`.

Uma implementação que pega primeiro o top-k global e depois filtra perde a
resposta correta. O oráculo independente filtra por categoria e haversine,
ordena pela similaridade cosseno e só então limita.

| Caso | RedDB | SurrealDB |
| --- | --- | --- |
| Top-1 elegível | `near_a` | `near_a` |
| Top-2 elegíveis | `near_a`, `near_b` | `near_a`, `near_b` |
| Pedir 3 quando existem apenas 2 elegíveis | exatamente 2 | exatamente 2 |
| Adicionar H3 no RedDB | mesma resposta | não é uma comparação de índices equivalentes |
| Explicar a consulta | topk, metadata_filter, vector_exact_scan; estimativas | TableScan, Compute, SortTopKByKey, Limit, projeção; métricas por operador |

O SurrealDB informa explicitamente `pre_decode_filter: no (unsupported predicate)`
e `topk_pushdown: no (unsupported order)`: nesse caso ele **também faz scan**.
Isso é bom diagnóstico, não demonstração de que seu índice ganhou. A amostra
não mede recall ANN, qualidade semântica de embeddings, RLS, grafo, escala,
concorrência ou recuperação.

## Onde aprender e onde diferenciar

| Dimensão | Evidência e avaliação | Consequência para RedDB |
| --- | --- | --- |
| Composição da linguagem | SurrealDB demonstrou documento + travessia de arestas na auditoria e vetor + geo + predicado neste probe. RedDB também passou no segundo caso e oferece SEARCH CONTEXT sobre várias estruturas. | Priorizar consultas compostas completas e hidratação dos resultados. Contar modelos ou dialetos não mede essa qualidade. |
| Plano versus execução | No caminho `runtime/query_exec/vector.rs`, os operadores `vector_ann_hnsw`, `vector_ann_ivf` e `vector_exact_scan` entram em `runtime_vector_matches`; fora do ramo TurboQuant, esse caminho monta `BruteForceVectorIndex` por consulta. Isso não significa que todo endpoint vetorial use esse caminho. | Certificar cada rota pública e fazer o EXPLAIN identificar o algoritmo realmente executado. Não anunciar custo de HNSW com base apenas no nome do plano ou na existência de um índice. |
| Busca filtrada | RedDB pré-filtra a busca exata; no ramo turbo com filtro, pede candidatos até `collection_count` e faz rerank exato. Na fonte SurrealDB, HNSW integra avaliação de condições, cache e pré-busca em lote de documentos. | Aprender a combinar seletividade e ANN sem depender de expansão até o tamanho da coleção. Medir recall e memória quando 99,9% dos candidatos são inelegíveis. |
| Observabilidade | O EXPLAIN FULL do SurrealDB retornou operadores, linhas, batches, tempo e motivos de otimizações não aplicadas. O EXPLAIN RedDB desta consulta retornou estimativas; o executor vetorial inicializa stats com Default. | Estimado versus realizado, candidatos visitados/descartados, uso efetivo de índice, memória e razão de fallback devem fazer parte do contrato. |
| Segurança da composição | RedDB tem `ReadFrame` com snapshot/escopo e `AuthorizedSearch` com restrição de coleções. A fonte SurrealDB verifica permissão SELECT antes da condição controlada pelo usuário dentro do filtro HNSW. | A autorização deve acompanhar o registro em cada expansão e preceder ranking/avaliação que possa revelar dados. O gate por coleção, sozinho, não certifica RLS por registro ou ausência de vazamento por contagem/ordem. |
| Transações entre modelos | RedDB documenta um frame comum e MVCC; SurrealDB possui uma autoridade transacional sobre backends. Nem este probe nem o fato de haver um WAL comum certificam isolamento entre documento, aresta e vetor. | Uma campanha com leitores concorrentes, rollback, tombstones, atualização dos índices e crash deve provar a mesma versão lógica em todas as projeções. |
| DX e operação | A auditoria encontrou dump que alterava valores, restore parcial bem-sucedido e resposta Bun sem registros. O novo probe encontrou vetor fracionário rejeitado. O SurrealDB passou no export/import lógico pequeno, mas o addon nativo examinado teve um problema de reabertura. | Fechar jornadas com os pacotes publicados e comparar valores completos, não apenas quantidade de linhas. Falhas do addon não devem ser atribuídas ao servidor. |
| Inteligência de aplicação | SEARCH CONTEXT e ASK com consultas somente de leitura e fontes são possibilidades reais de diferenciação do RedDB. Ainda falta avaliação equivalente de respostas fundamentadas, custo e negação de dados. | Medir acurácia, cobertura de citações e comportamento quando não existe evidência. Uma resposta confiante baseada em contexto errado piora a solução. |

## O que significa ser mais inteligente

Recomendação de arquitetura, ainda não uma decisão de reescrita: aprofundar o
contrato de execução canônica que já existe. Operadores especializados de
tabela, documento, grafo, vetor e geografia precisam consumir o mesmo contexto
de leitura/autorização e produzir registros tipados com identidade e origem.
O planejador escolhe a ordem dos filtros, expansões e ranking usando evidência
de seletividade; a execução informa o que realmente fez.

Uma consulta representativa é: “os melhores documentos sobre este assunto,
ligados a fornecedores aprovados, disponíveis perto do cliente, visíveis para
seu usuário”. Ela une predicados relacionais/documentais, grafo, geo e vetor.
O resultado precisa ter as mesmas identidades, campos, snapshot e permissões
independentemente do plano escolhido.

Isso permite aprender com os operadores em lote e com o HNSW filtrado do
SurrealDB sem escolher antecipadamente Arrow, outro WAL ou outro backend.

## Próximos gates, em ordem

1. **Contrato público confiável:** concluir #2272; incluir vetores reais fracionários,
   tipos exatos e respostas com/sem parâmetros em cada driver anunciado. Ainda
   falta o formato lógico completo de schema/índices/tenancy/permissões do P4.
2. **Executor verificável:** fixture por rota vetorial que compara plano, operador
   invocado e contadores reais. Testar coleção pequena/grande, filtro raro e
   índice ausente/obsoleto; fallback deve ser explícito e ter custo limitado.
3. **Corpus diferencial multimodelo:** ampliar este oráculo para grafo + vetor +
   documentos + geo; incluir updates/deletes, empates, valores ausentes,
   antimeridiano, concorrência e permissões por registro. Uma falha de
   correção bloqueia qualquer alegação de ganho de velocidade.
4. **Plano mais eficiente a qualidade igual:** medir scan exato, filtro antes/depois
   de ANN e expansão adaptativa com recall predefinido. Memória, cancelamento e
   p95/p99 entram no resultado, junto com latência/throughput.
5. **Aplicação inteligente demonstrada:** a mesma aplicação de recuperação nos
   dois bancos, com relevância julgada, origem dos resultados e custo operacional.
   Quando houver ASK, avaliar a resposta e as fontes separadamente do ranking.

A política da auditoria continua valendo na matriz completa: pelo menos 20%
no indicador primário escolhido previamente, intervalo de confiança de 95%
além do limiar e garantias/qualidade equivalentes, em host reservado para
declarar liderança. Este documento não reduz o objetivo a uma seleção de casos.

## Fontes primárias

- [RedDB: executor vetorial da base examinada](https://github.com/reddb-io/reddb/blob/85153296d8beba3bfa5ded2dfa719f416f50393d/crates/reddb-server/src/runtime/query_exec/vector.rs).
- [RedDB: frame de leitura](https://github.com/reddb-io/reddb/blob/85153296d8beba3bfa5ded2dfa719f416f50393d/crates/reddb-server/src/runtime/statement_frame.rs) e [busca autorizada](https://github.com/reddb-io/reddb/blob/85153296d8beba3bfa5ded2dfa719f416f50393d/crates/reddb-server/src/runtime/authorized_search.rs).
- [SurrealDB: execução em streams/batches](https://github.com/surrealdb/surrealdb/blob/93ab219d69f09d8f999851b0359c80ebe6726102/surrealdb/core/src/exec/mod.rs).
- [SurrealDB: pipeline de seleção e filtros](https://github.com/surrealdb/surrealdb/blob/93ab219d69f09d8f999851b0359c80ebe6726102/surrealdb/core/src/exec/planner/select/pipeline.rs).
- [SurrealDB: filtro HNSW, pré-busca e permissão antes da condição](https://github.com/surrealdb/surrealdb/blob/93ab219d69f09d8f999851b0359c80ebe6726102/surrealdb/core/src/idx/trees/hnsw/filter.rs).
- [SurrealDB: autoridade transacional e diferenças entre backends](https://github.com/surrealdb/surrealdb/blob/93ab219d69f09d8f999851b0359c80ebe6726102/surrealdb/core/src/kvs/tx.rs).
- RedDB: [ADR 0068, ASK](../adr/0068-ask-planner-first-redesign.md), [ADR 0065, MVCC](../adr/0065-transaction-manager-v2-rewrite.md). As descrições históricas dos ADRs não são tratadas como inventário atualizado de implementação.
