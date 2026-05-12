# AI batching: deprecate openai_embeddings sync legacy + final cleanup [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/281

Labels: enhancement

GitHub issue number: #281

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#272

## What to build

Cleanup final. Marca a API sync legacy como deprecated, garante que todos os call sites migraram, atualiza docs.

End-to-end:
- `ai::openai_embeddings` (sync) marcada `#[deprecated(since = "<v>", note = "use AiBatchClient::embed_batch")]`.
- Audit completo: garantir que **nenhum** call site interno chama mais a API sync. Se algum sobrar, migrar.
- `ai::openai_prompt` + `ai::anthropic_prompt` (sync) idem.
- Compile warning em build se algum site usa as funcs deprecated.
- Docs atualizadas: `docs/api/http.md`, `docs/data-models/vectors.md`, exemplos AUTO EMBED apontam para nova arquitetura (transparente para user).
- Release notes: changelog entry com speedup mensurado.

## Acceptance criteria

- [ ] Funcs sync marcadas deprecated.
- [ ] `cargo build -p reddb-server` produz zero warnings de deprecated em código próprio.
- [ ] Grep no código não encontra calls a `openai_embeddings` (sync) fora de tests/back-compat shims.
- [ ] Doc updates aplicadas.
- [ ] Changelog entry escrita.
- [ ] Final integration suite passa: INSERT 1000 rows AUTO EMBED + bulk HTTP + ASK + NER, todos via novo path.

## Blocked by

- #276
- (slice 7 prompts — #277 ou similar)
