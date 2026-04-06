# RedDB Architecture (Layered)

## Camadas implementadas

- `api.rs`
  - Contratos de alto nível (opções, capacidades, erros e traits estáveis)
  - `RedDBOptions`, `StorageMode`, `Capability`, `RedDBError`, `CatalogService`
  - Base para compatibilidade de API entre versões

- `engine.rs`
  - Fachada da camada física de armazenamento (pager/WAL/B-tree)
  - Entrada principal para modo persistente (`RedDBEngine::open`, `checkpoint`, `sync`, `stats`, `health`, `close`)
  - Encapsula `storage::engine::Database`

- `index.rs`
  - Contratos/telemetria de índices
  - Catálogo de índices em memória para configuração e health de camada de índices

- `health.rs`
  - Contratos de observabilidade
  - `HealthReport`, `HealthState`, `HealthIssue`, `HealthProvider`

## Compatibilidade

- Mantive `pub use crate::storage::*` em `lib.rs` para preservar exportações existentes.
- Adicionei novos exports em namespaces estáveis (`prelude`, `api`, `engine`, `index`, `health`).

## Próximo passo natural

- Substituir o corpo de `RedDBEngine::with_options` para usar um `PhysicalEngine` interno com seleção de estratégia (pager encriptado/físico simples/ memória).
- Conectar `IndexCatalog` ao pipeline real de criação/compaction para refletir métricas reais.
- Aplicar `features` do `Cargo.toml` em pontos de construção para controlar compilação por perfil.
