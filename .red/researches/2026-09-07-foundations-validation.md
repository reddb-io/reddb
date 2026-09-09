# Correções competitivas: validação e impacto

Data: 2026-09-07. Issue #2270; correções no PR #2272.

## Conclusão

As regressões de respostas públicas, vetores fracionários e restore escalar
foram corrigidas e verificadas. O append remove trabalho comprovadamente
redundante, mas **não resolve o piso de latência do #2270**. A melhoria varia
por cenário e não alcança o gate de liderança. O problema de desempenho e o
programa competitivo completo continuam abertos.

## Desempenho observado

Mediana das médias por execução, em microssegundos por escrita. Dez execuções
medidas por célula, após um aquecimento; um cliente espera cada INSERT. Payload
idêntico ao issue, SDK publicado 1.23.2, Bun 1.4.1. Cada execução verifica todas
as chaves e payloads antes e depois de reabrir o banco.

| Armazenamento | Linhas | RedDB antes | RedDB corrigido | SQLite WAL/FULL | Variação RedDB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Disco | 200 | 3540 | 3408 | 1366 | -3,7% |
| Disco | 1000 | 5110 | 4189 | 1056 | -18,0% |
| tmpfs | 200 | 1380 | 1215 | 32,8 | -11,9% |
| tmpfs | 1000 | 2953 | 3000 | 29,4 | +1,6% |

O bootstrap por execução dá IC95% da razão corrigido/antes de
[0,930; 1,006], [0,805; 0,844], [0,839; 0,965] e [0,941; 1,062],
respectivamente. Disco/200 e tmpfs/1000 não distinguem ganho de empate nesse
intervalo. Nenhum cenário tem todo o intervalo abaixo de 0,8.

A máquina estava compartilhada; as séries foram executadas em blocos
antes/depois, sem randomização. Esses intervalos descrevem variação entre
execuções, não eliminam mudanças de carga ou outros vieses sistemáticos.
Nossa compilação já havia terminado. RedDB usa SDK/subprocesso stdio, enquanto
SQLite roda no processo Bun: a tabela compara esses percursos de aplicação,
não apenas os motores. FULL é mais próximo da intenção durável que NORMAL,
mas não certifica equivalência de todas as garantias. tmpfs não prova
persistência após perda de energia.

A base RedDB é `85153296d8beba3bfa5ded2dfa719f416f50393d`; ambos os binários
foram construídos com o mesmo perfil `release`. Não confundir essa base
recompilada com o artefato estático publicado de v1.23.2. A versão corrigida
corresponde a `45a10e48a` (a última alteração antes desse commit foi somente
um comentário). Os hashes dos binários, dependências, revisão/hash do runner,
carga inicial, resultados por execução e bootstrap estão na
[evidência de desempenho](evidence/2026-09-07/foundations-performance.json).
As amostras individuais por chamada permanecem no scratch local
`/tmp/reddb-competitive-audit/results/foundations`; o arquivo versionado
preserva as métricas por execução usadas nos cálculos e os hashes dos originais.

Reprodução no diretório `competitive` do benchmark PR #2:

```sh
REDDB_BIN=/absolute/path/to/red bun run audit.mjs --engine reddb \
  --storage disk --root /tmp/reddb-measurement --items 1000 --runs 10 --output red.json
bun run audit.mjs --engine sqlite-full --storage disk \
  --root /tmp/sqlite-measurement --items 1000 --runs 10 --output sqlite.json
python3 analyze.py red.json sqlite.json --metric latency --output comparison.json
```

Repetir RedDB com cada binário e usar `--storage tmpfs --root /dev/shm/...`
para a outra célula. A raiz precisa corresponder ao armazenamento anunciado.

## Mecanismo e durabilidade

Um rastreamento separado de 200 INSERTs confirmou os mesmos **202 fdatasync e
201 fsync** antes e depois. `openat` caiu de 403 para 203; bytes de `read`
concluídos, somados sobre todos os descritores, de 17.027.149 para 10.149.758.
Não atribuímos todo byte ao WAL nem usamos latência instrumentada na tabela.
[Contagens e limites temporais](evidence/2026-09-07/foundations-syscalls.json).
Isso comprova redução de I/O redundante mantendo as barreiras observadas;
não prova que o restante do custo venha de um único subsistema.

A regressão de corrupção falha na base anterior: após adulteração do WAL
publicado, o append retorna sucesso. Na versão corrigida ele recusa a escrita
até reparo. Testes também cobrem outro processo, checkpoint, troca de pathname,
troca com superblocos idênticos, concorrência e pontos de crash.

## Verificações funcionais

- `cargo check` e build otimizado concluídos.
- 428 testes do crate de arquivo e 84 testes do cliente Rust sem features padrão.
- Clippy do crate de arquivo (`--lib --no-deps -- -D warnings`) passou. Incluir
  dependências encontra `question_mark` preexistente em reddb-types; não é uma
  afirmação de clippy limpo para o workspace inteiro.
- Três programas Bun: SDK compartilhado, negociação legada e cliente RedWire
  real. A regressão vetorial verifica score 1 para o vetor fracionário idêntico
  e 0,8 contra o eixo, além de inserção bem-sucedida.
- Cinco regressões CLI: valores escalares/reabertura, falha parcial em texto e
  JSON, identificador inválido, números exatos e histórico de configuração.
- Smoke compilado contra a biblioteca release, baseado no teste existente
  `cli_dump_restore_includes_plaintext_config_and_encrypted_vault_kv`: vault
  criptografado e valor de configuração sobrevivem ao CLI dump/restore e à
  reabertura; plaintext da fixture não aparece no export. Não foi executado
  o harness inteiro de configuração nesse smoke.
- 200 escritas reconhecidas sobrevivem a SIGKILL; todos os valores também
  sobrevivem a dump/restore em destino novo. É crash de processo, não de energia.
- Dez casos do oráculo multimodelo passaram nos dois motores, incluindo a
  regressão específica de parâmetro vetorial no RedDB.

[Manifesto de validação](evidence/2026-09-07/foundations-validation.json),
[recuperação](evidence/2026-09-07/foundations-recovery.json) e
[avaliação multimodelo](2026-09-07-multimodel-quality.md).
Os logs de arquivo, cliente, CLI e Bun estão junto ao manifesto. Os testes CLI
Bun foram adicionados ao job existente Driver Runnable Examples; esse job
requer `workflow_dispatch` com `full_ci`, portanto um check de PR verde não
significa que esse job foi executado.

## O que permanece

O próximo diagnóstico de latência deve decompor o trabalho restante por
statement e por tamanho de coleção, incluindo snapshot, índices e execução.
A tabela não isola qual deles explica o crescimento. Reduzir garantias de
commit ou recomendar somente batch não satisfaz o caso de um evento por escrita.

Na qualidade multimodelo, a prioridade é tornar verificável a correspondência
entre plano e algoritmo executado; ampliar o oráculo para grafo, updates/deletes,
concorrência e RLS; então otimizar busca filtrada com recall equivalente.
O export lógico completo de schema/índices/tenancy/permissões, a semântica geral
de identificadores entre aspas e a conformidade completa de aridade dos
parâmetros continuam pendentes. Nenhuma dessas frentes é declarada concluída
por este PR.
