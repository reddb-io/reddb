---
"@reddb-io/cli": patch
---

Fix missing RedWire CLI SELECT rows by using the existing typed full-result query path instead of interpreting a summary-only reply as rows. Pin actual column/value output in the cross-binary regression for all row formats. Includes the accumulated 1.23.3 changes, whose publication was stopped during artifact verification; see the v1.23.4 upgrade notes for persisted expression/function compatibility and memory-evidence limits.
