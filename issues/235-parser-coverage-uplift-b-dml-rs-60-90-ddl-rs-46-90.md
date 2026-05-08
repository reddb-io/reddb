# Parser coverage uplift B: dml.rs (60→90%) + ddl.rs (46→90%) [AFK]

## Parent

#227

## What to build

Subir cobertura de:

- `parser/dml.rs`: 60.13% → ≥90%. Cobrir: INSERT…RETURNING, INSERT…WITH TTL/EXPIRES AT/METADATA/AUTO EMBED, UPDATE…SET com subquery, UPDATE…WHERE com filtros complexos, DELETE…RETURNING, INSERT VALUES multi-row, INSERT INTO ... DOCUMENT/KV/NODE/EDGE/VECTOR (todas variantes), parsing de SECRET/PASSWORD literais.
- `parser/ddl.rs`: 46.50% → ≥90%. Cobrir: CREATE TABLE com todas as constraints (PRIMARY KEY, NOT NULL, UNIQUE, DEFAULT, REFERENCES), CREATE INDEX (B-tree, hash, partial, expression), CREATE VIEW, ALTER TABLE (ADD/DROP/RENAME COLUMN, RENAME TO), DROP TABLE/INDEX/VIEW com IF EXISTS, CREATE TIMESERIES/QUEUE/VECTOR DDL paths.

## Acceptance criteria

- [ ] `dml.rs` ≥90% lines.
- [ ] `ddl.rs` ≥90% lines.
- [ ] Cada constraint/clause documentada em `docs/query/*.md` tem ao menos 1 teste.
- [ ] Workflow #228 reporta os novos números no PR.
- [ ] Sem regressão em `cargo test -p reddb-server`.

## Blocked by

- #228
- #229
