# Parser Fuzzing

Nightly parser fuzzing runs `sql_parser`, `migration_parser`, and
`conn_string_parser` from `.github/workflows/parser-fuzz-nightly.yml`.

## Local Run

```bash
cargo +nightly install cargo-fuzz --locked
python3 scripts/seed-parser-fuzz-corpus.py
cargo +nightly fuzz run sql_parser -- -max_total_time=60
cargo +nightly fuzz run migration_parser -- -max_total_time=60
cargo +nightly fuzz run conn_string_parser -- -max_total_time=60
```

## Reproduce a Crash

Download the workflow artifact, place the reproducer under
`fuzz/artifacts/<target>/`, then run:

```bash
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<artifact>
cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/<artifact> -- -max_total_time=60
```

If the minimized input becomes a regression test, add it to the matching parser
test or to `crates/reddb-server/tests/conformance/negative/`.

## Corpus

The workflow seeds `fuzz/corpus/sql_parser` from the parser conformance TOMLs on
every run, then restores and uploads the evolved corpus per target. The corpus is
ignored in git because it is generated and can grow quickly.
