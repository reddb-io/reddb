# Plano — Drivers Nativos RedDB

## Objetivo

Entregar 4 drivers nativos oficiais (Rust, Node/Bun/Deno, Python) com API
consistente, instalação trivial e versão sempre alinhada à engine. O usuário
final deve precisar de **um único comando** por linguagem para começar a usar
RedDB, e a mesma `connect(uri)` deve abrir um arquivo embedded, um servidor
remoto, uma replica ou modo memória.

Este documento é o contrato. Quem implementa qualquer fase olha primeiro aqui.

## Princípios

1. **Single source of truth no binário.** O `red` (Rust) implementa o
   protocolo, transports, auth, replication, retry. Nenhum driver reimplementa
   nada disso.
2. **Drivers JS/Python são thin wrappers**: spawn do binário em modo stdio +
   parsing JSON. Drivers Rust acessam a engine direto (sem subprocess).
3. **Mesma API em todas as linguagens.** Connection-string única, mesmos
   métodos, mesmos códigos de erro. Quem aprende um driver sabe os outros.
4. **Zero-friction install.** `pnpm add reddb` / `pip install reddb` /
   `cargo add reddb-client`. Postinstall baixa o binário pra
   `node_modules/reddb/bin/red` quando necessário. Sem PATH, sem build local.
5. **Versionamento alinhado.** Engine, drivers e CLI saem todos com a mesma
   versão a cada release.
6. **Honestidade sobre limites.** stdio+JSON tem overhead de IPC. Quem precisa
   de perf máxima usa a crate Rust embedded. Documentar.

## Arquitetura

```
┌─────────────────────────────────────────────────────────────────────┐
│  Aplicação do usuário                                               │
│  ──────────────────────                                             │
│  reddb.connect("grpc://host:50051")  /  connect("file:///data.rdb") │
└─────────────┬───────────────────────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────┐    ┌──────────────────────────────────┐
│  Driver JS / Python         │    │  Driver Rust                     │
│  (subprocess + JSON-RPC)    │    │  (in-process)                    │
└──────────┬──────────────────┘    └────────────┬─────────────────────┘
           │ stdio / line-delim JSON-RPC 2.0    │ direct API calls
           ▼                                    ▼
┌──────────────────────────────────────────────────────────────────────┐
│  red (binário)                                                       │
│  ─────────────                                                       │
│  modos: red rpc --stdio  |  red server  |  red replica  |  red ...   │
│  resolve uri → engine embedded | tonic-client | replica stream       │
└─────────────┬───────────────────────────────┬────────────────────────┘
              │                               │
              ▼                               ▼
   ┌──────────────────────┐         ┌────────────────────┐
   │  arquivo .rdb local  │         │  servidor remoto   │
   │  (engine embedded)   │         │  via gRPC tonic    │
   └──────────────────────┘         └────────────────────┘
```

## Decisões fixadas

| Item                                | Decisão                                                                 |
|-------------------------------------|-------------------------------------------------------------------------|
| Protocolo driver↔binário (JS/Py)    | JSON-RPC 2.0 line-delimited sobre stdin/stdout                          |
| Protocolo entre binários            | gRPC puro (já existe via tonic)                                         |
| Driver JS: pacote                   | Único: `reddb` no npm. Roda em Node, Bun, Deno (subprocess unificado).  |
| Driver JS: Cloudflare Workers/Edge  | **Não** suportado nesta fase. Sem subprocess no Workers.                |
| Driver Python                       | Mantém pyo3+maturin (in-process via FFI), não subprocess                |
| Driver Rust                         | Sempre direto: embedded p/ `file://`/`memory://`, tonic p/ `grpc://`    |
| Postinstall (JS)                    | Baixa binário pra `node_modules/reddb/bin/red`, igual `reddb-cli` faz   |
| PyPI nome                           | `reddb` (verificar disponibilidade antes; fallback `reddb-client`)      |
| PyPI publish                        | Job no CI **comentado** — `PYPI_API_TOKEN` é criado depois              |
| Naming `sdk/` vs `drivers/`         | Mantém como está. Sem rename.                                           |
| Termo "client" vs "SDK"             | Mantém como está. Sem padronização forçada.                             |

## Spec do protocolo stdio (JSON-RPC 2.0 line-delimited)

### Encoding

- Cada request e cada response ocupam **uma linha** UTF-8 terminada em `\n`.
- JSON values **não podem** conter newlines literais (use `\n` escapado).
- Pipelining permitido: cliente pode mandar N requests antes de receber a
  primeira resposta. O binário responde em ordem de chegada (não garante
  ordem por id se o cliente paralelizar internamente; o `id` é a referência).
- Server-side:
  - stdin EOF → fecha graciosamente (flush + exit 0).
  - SIGTERM → mesmo.
  - Erro fatal → escreve `{"jsonrpc":"2.0","error":{...}}` em stdout e exit !=0.

### Request

```json
{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"SELECT * FROM users LIMIT 10"}}
```

Campos:
- `jsonrpc`: sempre `"2.0"`.
- `id`: inteiro ou string. O cliente é responsável pela unicidade.
- `method`: string (ver tabela abaixo).
- `params`: objeto. Schema depende do método.

### Response (success)

```json
{"jsonrpc":"2.0","id":1,"result":{"rows":[{"id":1,"name":"Alice"}],"affected":0}}
```

### Response (error)

```json
{"jsonrpc":"2.0","id":1,"error":{"code":"PARSE_ERROR","message":"unexpected token at position 12","data":null}}
```

Códigos de erro (string, estáveis — drivers podem mapear pra exceptions
idiomáticas):

| code              | quando                                                       |
|-------------------|--------------------------------------------------------------|
| `PARSE_ERROR`     | JSON inválido na linha                                       |
| `INVALID_REQUEST` | Falta campo obrigatório, método desconhecido                 |
| `INVALID_PARAMS`  | params não bate com schema do método                         |
| `QUERY_ERROR`     | Erro de SQL (parse, type, constraint)                        |
| `IO_ERROR`        | Falha de disco, rede, permissão                              |
| `NOT_FOUND`       | Entity ou collection não existe                              |
| `AUTH_ERROR`      | Token inválido, role insuficiente                            |
| `INTERNAL_ERROR`  | Bug no binário (panic capturado, etc)                        |

### Métodos (v1)

| Método         | Params                                                                 | Result                                          |
|----------------|------------------------------------------------------------------------|-------------------------------------------------|
| `query`        | `{"sql": string, "params": [..] (opcional)}`                           | `{"rows": [..], "columns": [..], "affected": int}` |
| `insert`       | `{"collection": string, "payload": object}`                            | `{"id": string, "affected": 1}`                 |
| `bulk_insert`  | `{"collection": string, "payloads": [object]}`                         | `{"affected": int}`                             |
| `get`          | `{"collection": string, "id": string}`                                 | `{"entity": object \| null}`                    |
| `delete`       | `{"collection": string, "id": string}`                                 | `{"affected": int}`                             |
| `health`       | `{}`                                                                   | `{"ok": true, "version": string}`               |
| `version`      | `{}`                                                                   | `{"version": string, "protocol": "1.0"}`        |
| `close`        | `{}`                                                                   | `null`  (binário fecha após responder)          |

Métodos podem crescer em fases futuras. Drivers devem tolerar `result` com
campos extras (forward compat). Servidor deve retornar `INVALID_REQUEST` pra
método desconhecido (não ignorar silenciosamente).

### Connection string

URIs aceitas pelo `red rpc --stdio` (via flag `--connect`):

| URI                              | Significado                                              |
|----------------------------------|----------------------------------------------------------|
| `file:///path/to/data.rdb`       | Abre engine embedded no arquivo                          |
| `memory://`                      | Engine in-memory, não persiste                           |
| `grpc://host:50051`              | Cliente tonic conectando num servidor remoto             |
| `grpc://host:50051?auth=<token>` | Mesmo, com bearer token                                  |

Drivers convertem o que o usuário passou em `connect(uri)` direto pro
`--connect` do binário. Não precisam parsear URI eles mesmos — só repassam.

---

## Fase 0 — Spec do protocolo (esse documento)

**Status:** ✅ feito (esta seção).

**Saída:** este `PLAN_DRIVERS.md`. Drivers e binário implementam contra esta
spec.

---

## Fase 1 — `red rpc --stdio` no binário

**Objetivo:** dar ao binário um modo daemon stdio que fala JSON-RPC 2.0.

**Mudanças:**

- `src/cli/types.rs`: novo enum variant `Command::Rpc { mode: RpcMode, connect: Option<String>, path: Option<String>, ... }`.
- `src/bin/red.rs`: dispatch `rpc` → handler novo.
- `src/server/rpc_stdio.rs` (novo arquivo): loop principal.
  - `BufReader<Stdin>::lines()`.
  - Parse com `serde_json::from_str::<JsonRpcRequest>`.
  - Match no `method`, despacha pros executors existentes (mesmos que `red query`, `red insert`, etc usam).
  - Serializa resposta com `serde_json::to_string` + `println!` + `stdout().flush()`.
  - Captura panic via `std::panic::catch_unwind`, devolve `INTERNAL_ERROR`.
- `src/lib.rs`: re-export do módulo se necessário.
- Reaproveita `RuntimeContext` que `server` já constrói.
- Suporta as mesmas flags do `red server`: `--path`, `--connect`, `--vault`, `--read-only`.

**Testes:**

- `tests/integration_rpc_stdio.rs`: spawna `red rpc --stdio --path :memory:`, manda 5 requests pipelinadas, valida respostas em ordem.
- Caso de erro: JSON inválido → `PARSE_ERROR`.
- Caso de erro: método desconhecido → `INVALID_REQUEST`.
- Caso `close` → binário sai com código 0.

**Critério de aceite:**

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"version","params":{}}' | red rpc --stdio --path :memory:
# {"jsonrpc":"2.0","id":1,"result":{"version":"0.1.x","protocol":"1.0"}}
```

---

## Fase 2 — Driver JS `reddb` (npm)

**Objetivo:** pacote único `reddb` que roda em Node 18+, Bun, Deno via npm
specifier.

**Layout novo:** `drivers/js/`
- `package.json` (`name: "reddb"`, `main: "dist/index.js"`, `types: "dist/index.d.ts"`)
- `src/index.ts` — API pública (`connect`, `RedDB` class)
- `src/spawn.ts` — runtime detection + process spawn
- `src/protocol.ts` — JSON-RPC encode/decode + pending requests map
- `postinstall.js` — copiado de `sdk/postinstall.js`, baixa o binário pra `bin/red`
- `test/smoke.test.js` — spin up binário, abre conexão, faz query, fecha
- `tsconfig.json`, `.npmignore`

**Runtime detection (`src/spawn.ts`):**

```ts
function spawnRed(args: string[]): ChildProcessLike {
  if (typeof Bun !== 'undefined' && Bun.spawn) {
    return adaptBun(Bun.spawn({ cmd: ['red', ...args], stdin: 'pipe', stdout: 'pipe' }))
  }
  if (typeof Deno !== 'undefined' && Deno.Command) {
    return adaptDeno(new Deno.Command('red', { args, stdin: 'piped', stdout: 'piped' }).spawn())
  }
  const { spawn } = require('node:child_process')
  return spawn('red', args, { stdio: ['pipe', 'pipe', 'inherit'] })
}
```

**API pública:**

```ts
import { connect } from 'reddb'

const db = await connect('file:///data.rdb')
const result = await db.query('SELECT * FROM users LIMIT 10')
const id = await db.insert('users', { name: 'Alice' })
await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])
await db.close()
```

**Detalhes técnicos:**

- Pending requests map: `Map<id, { resolve, reject }>`. Cada `query()` aloca id auto-incremento, escreve linha, registra promise, espera resposta.
- Single reader: `readline.createInterface({ input: child.stdout })` na linha `'line'` faz lookup no map.
- Erros: response com `error` rejeita a promise com `RedDBError` (classe própria com `code`, `message`, `data`).
- `close()`: manda método `close`, espera resposta, mata o processo.
- Auto-close em `process.on('exit')` via `WeakRef` ou cleanup explícito.

**Postinstall:**

- Detecta `process.platform` + `process.arch`.
- Mapa de targets:
  - `linux x64`  → `red-linux-x86_64`
  - `linux arm64`→ `red-linux-aarch64`
  - `darwin arm64` → `red-macos-aarch64`
  - `win32 x64`  → `red-windows-x86_64.exe`
- Baixa de `https://github.com/forattini-dev/reddb/releases/download/v<version>/<asset>`.
- `chmod +x bin/red` no Unix.
- Falha graciosa: avisa que o usuário precisa instalar `red` manualmente, **não** quebra o `npm install`.

**Critério de aceite:**

- `pnpm add reddb` → instala, postinstall baixa o binário.
- `node test.js` faz query, insert e fecha sem erro.
- Mesmo teste em Bun e Deno.

---

## Fase 3 — Driver Rust `reddb-client` (crates.io)

**Objetivo:** crate fina, idiomática, async-first, com dois modos transparentes
(embedded e remote).

**Layout novo:** `drivers/rust/`
- `Cargo.toml` (`name = "reddb-client"`)
- `src/lib.rs` — `pub use connect::*; pub use error::*;`
- `src/connect.rs` — `Reddb::connect(uri)` parser e dispatch
- `src/embedded.rs` — wrapper sobre `reddb::engine` (re-exporta a engine)
- `src/grpc.rs` — cliente tonic gerado de `proto/`
- `src/error.rs` — `RedDBError` com `code` enum
- `tests/smoke.rs`

**API:**

```rust
use reddb_client::Reddb;

let db = Reddb::connect("file:///data.rdb").await?;
let rows = db.query("SELECT * FROM users LIMIT 10").await?;
let id = db.insert("users", json!({"name": "Alice"})).await?;
db.close().await?;
```

**Features:**

```toml
[features]
default = ["embedded", "grpc"]
embedded = ["dep:reddb"]
grpc = ["dep:tonic", "dep:prost"]
```

Quem só precisa do cliente remoto desliga `embedded` e fica sem o overhead de compilar a engine inteira.

**Dispatch:**

```rust
match Url::parse(uri)?.scheme() {
    "file" => Reddb::Embedded(EmbeddedDb::open(path)?),
    "memory" => Reddb::Embedded(EmbeddedDb::in_memory()?),
    "grpc" => Reddb::Remote(GrpcClient::connect(host).await?),
    other => bail!("unsupported scheme: {other}"),
}
```

`Reddb` é enum, métodos delegam pro variant. Trait pode ser usado se quiser
abstrair, mas enum dispatch é mais simples e não precisa Box.

**Critério de aceite:**

- `cargo test -p reddb-client` passa com os 3 schemes.
- Compila com `--no-default-features --features grpc` (cliente puro).
- Documentação inline com `///` em todos os pub items.

---

## Fase 4 — Driver Python `reddb` (PyPI, pyo3)

**Objetivo:** wheel pré-compilada por arch, instalação via `pip install reddb`,
zero compilação local. Mantém a abordagem in-process via pyo3.

**Mudanças:**

- `drivers/python/` já existe. Refatorar API pra bater com a spec:
  - `reddb.connect(uri)` em vez de `connect(addr)`.
  - Métodos: `query`, `insert`, `bulk_insert`, `get`, `delete`, `close`.
  - Suportar `file://`, `memory://`, `grpc://` no `connect`.
- `pyproject.toml`: bump pra `name = "reddb"` (verificar PyPI antes), version sync.
- `pyo3` já está em `0.24` (fechamos o RUSTSEC).

**CI — `release.yml` job novo `publish-python` (COMENTADO):**

```yaml
# publish-python:
#   name: PyPI wheels
#   needs: [plan, build]
#   if: needs.plan.outputs.should_skip != 'true'
#   strategy:
#     matrix:
#       include:
#         - { os: ubuntu-latest, target: x86_64-unknown-linux-gnu, manylinux: 2014 }
#         - { os: ubuntu-latest, target: aarch64-unknown-linux-gnu, manylinux: 2014 }
#         - { os: macos-14, target: aarch64-apple-darwin }
#         - { os: macos-13, target: x86_64-apple-darwin }
#         - { os: windows-latest, target: x86_64-pc-windows-msvc }
#   runs-on: ${{ matrix.os }}
#   steps:
#     - uses: actions/checkout@v4
#     - uses: actions/setup-python@v5
#       with: { python-version: '3.x' }
#     - uses: PyO3/maturin-action@v1
#       with:
#         working-directory: drivers/python
#         target: ${{ matrix.target }}
#         manylinux: ${{ matrix.manylinux || 'auto' }}
#         args: --release --out dist
#     - uses: actions/upload-artifact@v4
#       with: { name: wheels-${{ matrix.target }}, path: drivers/python/dist }
#
# publish-python-upload:
#   needs: [publish-python]
#   runs-on: ubuntu-latest
#   steps:
#     - uses: actions/download-artifact@v4
#       with: { pattern: wheels-*, merge-multiple: true, path: dist }
#     - uses: PyO3/maturin-action@v1
#       with:
#         command: upload
#         args: --skip-existing dist/*
#       env:
#         MATURIN_PYPI_TOKEN: ${{ secrets.PYPI_API_TOKEN }}
#
# TODO: criar secret PYPI_API_TOKEN no repo e descomentar este bloco.
# Verificar que `reddb` está disponível no PyPI antes do primeiro publish.
# Se estiver tomado, fallback name = "reddb-client" no pyproject.toml.
```

**Critério de aceite:**

- `cd drivers/python && maturin develop` builda local.
- `python -c "import reddb; db = reddb.connect('memory://'); print(db.query('SELECT 1'))"` funciona.
- Job CI comentado, com TODO claro.

---

## Fase 5 — Atualizar `release.yml`

**Objetivo:** publicar drivers Rust e JS automaticamente em cada release. Python
fica comentado (Fase 4).

**Jobs novos:**

### `publish-rust-client`

```yaml
publish-rust-client:
  name: Publish reddb-client (crates.io)
  needs: [plan, publish-cargo]
  runs-on: ubuntu-latest
  if: |
    needs.plan.outputs.should_skip != 'true' &&
    needs.plan.outputs.release_channel == 'stable'
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@1.91.0
    - name: Sync version
      run: scripts/sync-version.js ${{ needs.plan.outputs.package_version }}
    - name: Publish
      working-directory: drivers/rust
      env:
        CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
      run: cargo publish --allow-dirty
```

Roda **depois** de `publish-cargo` se `reddb-client` depender de `reddb`. Se for
independente (sem feature `embedded`), pode ser paralelo.

### `publish-js-driver`

```yaml
publish-js-driver:
  name: Publish reddb (npm)
  needs: [plan, publish-github]
  runs-on: ubuntu-latest
  if: needs.plan.outputs.should_skip != 'true'
  steps:
    - uses: actions/checkout@v4
    - uses: actions/setup-node@v4
      with: { node-version: '22', registry-url: 'https://registry.npmjs.org' }
    - name: Sync version
      run: scripts/sync-version.js ${{ needs.plan.outputs.package_version }}
    - name: Build
      working-directory: drivers/js
      run: pnpm install --frozen-lockfile && pnpm build
    - name: Publish
      working-directory: drivers/js
      env:
        NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
      run: |
        TAG=$([[ "${{ needs.plan.outputs.release_channel }}" == "next" ]] && echo "next" || echo "latest")
        npm publish --tag "${TAG}" --access public
```

Precisa rodar **depois** de `publish-github` porque o postinstall do pacote
baixa o binário da Release recém-criada. Se publicar antes, postinstall quebra.

### `publish-python` (comentado)

Ver Fase 4.

---

## Fase 6 — Sync de versão + docs

**`scripts/sync-version.js`** passa a escrever em:

- `Cargo.toml` (engine — já faz)
- `package.json` raiz (já faz)
- `drivers/rust/Cargo.toml`
- `drivers/js/package.json`
- `drivers/python/Cargo.toml`
- `drivers/python/pyproject.toml`

Validação: depois de escrever, lê de volta e compara. Falha se algum arquivo
ficou desincronizado.

**Docs:**

- `README.md` raiz: tabela "Install":

  | Linguagem | Comando |
  |-----------|---------|
  | Rust      | `cargo add reddb-client` |
  | Node      | `pnpm add reddb` |
  | Bun       | `bun add reddb` |
  | Deno      | `import { connect } from 'npm:reddb'` |
  | Python    | `pip install reddb` |

- `docs/clients/rust.md`, `docs/clients/javascript.md`, `docs/clients/python.md`:
  cada um com:
  1. Install
  2. Connection strings (`file://`, `memory://`, `grpc://`)
  3. Exemplo CRUD básico
  4. Tratamento de erros
  5. Tabela de mapeamento `error.code` → exception idiomática
  6. Limites (perf de IPC pra JS/Py, edge runtimes não suportados, etc)

- `docs/protocol/stdio.md`: extrai a seção "Spec do protocolo stdio" deste plano
  pra um doc dedicado, pra quem quiser implementar driver de outra linguagem
  (Go, Ruby, Java, etc no futuro).

---

## Ordem de execução

| Fase | Depende de | Pode paralelizar com   | Estimativa            |
|------|------------|------------------------|-----------------------|
| 0    | —          | —                      | ✅ feito              |
| 1    | 0          | —                      | 1 sessão              |
| 2    | 1          | 3, 4                   | 1 sessão              |
| 3    | 0 (não 1)  | 1, 2, 4                | 1 sessão              |
| 4    | 0          | 1, 2, 3                | 1 sessão              |
| 5    | 1, 2, 3, 4 | —                      | 0.5 sessão            |
| 6    | 5          | —                      | 0.5 sessão            |

Fase 3 (Rust) **não depende** da Fase 1 porque o driver Rust fala direto com a
engine ou com tonic, sem usar o modo stdio. Pode ser feita primeiro se quiser
ver resultado rápido.

## Riscos identificados

| Risco                                                                | Mitigação                                                                              |
|----------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| Overhead de IPC stdio mata casos de alta vazão                       | Documentar limite. Usuário hot vai pra crate Rust embedded.                            |
| Postinstall falha em ambiente offline / restritivo (corp proxy)      | Postinstall não-bloqueante: avisa, sugere `REDDB_BINARY_PATH` env var, segue a vida.   |
| `reddb` no PyPI já está tomado                                       | Verificar antes da Fase 4. Fallback `reddb-client`.                                    |
| Bun/Deno divergência sutil em `child_process`                        | Camada `spawn.ts` com adapter por runtime + smoke test em cada um na CI.               |
| `red rpc --stdio` panica e deixa zombie process                      | `catch_unwind` no loop, testes de fault injection, drivers fazem `kill -9` em close()  |
| Versões dos drivers desalinhando                                     | Fase 6: `sync-version.js` valida + falha o build se houver drift.                      |
| Mudança no protocol JSON-RPC quebra drivers antigos                  | `version` retorna `protocol: "1.0"`. Bumps de protocol são major. Drivers checam.      |

## Não-objetivos (explícitos)

- **Cloudflare Workers / Vercel Edge / browser**: fora desta fase. Sem subprocess,
  precisa de transport HTTP separado. v2.
- **Drivers Go, Ruby, Java, .NET**: a spec stdio está documentada, mas não vamos
  implementar agora. Comunidade ou fase futura.
- **Connection pooling no driver JS/Py**: cada `connect()` spawna um processo. Pool
  fica como otimização posterior se virar gargalo.
- **Streaming results**: queries grandes hoje retornam o resultado inteiro de uma
  vez. Streaming via stdio é viável (cada chunk vira uma linha) mas fica fora do
  v1.
- **Renomear `sdk/`**: confirmado que fica como está.
