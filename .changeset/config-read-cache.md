---
"@reddb-io/cli": patch
---

Configuration reads are cached per state of `red.config`: statements no longer queue behind a checkpoint or a long index read just to check a setting.
