# CLI logical JSON export

```sh
red dump --path source.rdb -o rows.jsonl
red restore --path restored.rdb -i rows.jsonl
```

Row fields use RedDB's canonical JSON representation. Text is exported as its
actual contents, booleans and null remain JSON scalars, and exact large integers
and decimals use `$int`, `$uint` and `$decimal` envelopes. Restore uses the shared parameter decoder and a typed INSERT through the
runtime execution frame, preserving field names, quotes, backslashes and
Unicode without SQL-literal escaping. Earlier dumps containing SQL-formatted strings cannot be
unambiguously repaired: regenerate them from the original database.

Virtual `red`, `red.*` and `__red_schema_*` collections are excluded because
they are rebuilt by the runtime and cannot be inserted into. `red_config` stays
included, including when `--collection` selects another collection. A full dump
retains the encrypted vault-KV record; its existing certificate requirements
still apply. Collection overrides do not rename `red_config`.

Restore continues past bad records, checkpoints successful imports, reports
line errors, and exits nonzero if any record failed. In `--json` mode a partial
restore emits the CLI error envelope on stderr. It is not an atomic import:
use a fresh destination and check the exit status before using it.

This correction certifies scalar JSON row values, not complete database backup fidelity.
Schema/index definitions, tenancy and permissions are not exported by this
format. Canonical JSON also does not retain every storage-specific value type
(for example UUID versus text, or timestamp versus integer). Numeric arrays
follow the shared parameter contract and bind as f32 vectors; this is not a
lossless general JSON-document backup format. Non-row multimodel
entities are not a supported round-trip. Use the physical backup/recovery tools
when those properties must be preserved. A complete versioned logical backup
format remains part of the competitive audit's P4 work.

Regression command (build `red` first):

```sh
REDDB_BINARY_PATH=/absolute/path/to/red python3 tests/cli_dump_restore.py
```
