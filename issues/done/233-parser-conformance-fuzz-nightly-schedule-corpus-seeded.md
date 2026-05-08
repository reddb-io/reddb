# Parser conformance: fuzz nightly schedule + corpus seeded [AFK]

## Parent

#227

## What to build

Workflow GitHub Actions schedule (cron nightly) que roda os fuzz targets existentes em `reddb/fuzz/` por 1h cumulativo. Crashes/panics abrem issue automática com label `release-blocker`. Corpus persistido entre runs em branch separada.

Targets já existentes (não criar novos nesta slice):
- `sql_parser`
- `migration_parser`
- `conn_string_parser`

Seedar corpus inicial a partir do conformance corpus (#229) para que o fuzzer comece de inputs válidos e mute a partir daí.

## Acceptance criteria

- [ ] `.github/workflows/parser-fuzz-nightly.yml` criado, schedule cron diário.
- [ ] Roda `cargo fuzz run sql_parser -- -max_total_time=3600` e equivalentes para os 3 targets.
- [ ] Corpus persistido (artifact ou branch dedicada `fuzz-corpus`).
- [ ] Crash/panic detectado → cria issue automática com label `release-blocker`, body inclui input minimizado.
- [ ] Seedado com conformance corpus de #229 na primeira corrida.
- [ ] Documentado em `fuzz/README.md` ou `crates/reddb-server/AGENTS.md`: como reproduzir crash localmente, como adicionar input ao corpus.

## Blocked by

- #229
