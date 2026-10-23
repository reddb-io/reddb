---
"@reddb-io/cli": minor
---

Standardize the container default at `/data/red/database.rdb`, keeping `/data`
as the persistent volume. Explicit `--path` and `REDDB_DATA_PATH` overrides
remain supported. Existing volumes using the previous default must pin
`REDDB_DATA_PATH=/data/data.rdb` before upgrading, or migrate the complete
native file family with all writers stopped; changing the default does not
migrate existing data.
