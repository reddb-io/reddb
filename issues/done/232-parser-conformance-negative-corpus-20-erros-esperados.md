# Parser conformance: negative corpus (~20 erros esperados) [AFK]

## Parent

#227

## What to build

Adicionar casos negativos: inputs que devem falhar, com substring esperada da mensagem de erro. Garante que regressões em mensagens de erro (ou aceitação acidental de syntax inválida) sejam detectadas.

Fontes:
- Mensagens em `parser/error.rs` — cada variante de `ParseError` deve ter ao menos 1 caso disparando.
- Bug fixes em commits anteriores que envolveram parser — pegar o input que regrediu.
- Edge cases conhecidos: unclosed string, JSON literal aninhado >max_depth, queue push sem nome, INSERT sem VALUES, etc.

## Acceptance criteria

- [ ] ≥20 casos negativos em `tests/conformance/negative/`.
- [ ] Cada caso especifica `expected_error_substring` validado contra `ParseError::to_string()`.
- [ ] Cobre: depth limit, unclosed string/json, missing required token, reserved keyword as ident, invalid number literal, malformed JSON in literal, queue command incompleto.
- [ ] `cargo test -p reddb-server --test conformance` passa (todos os negativos falham conforme esperado).
- [ ] Cada variante de `ParseError` em `parser/error.rs` é exercitada por ao menos 1 caso negativo.

## Blocked by

- #229
