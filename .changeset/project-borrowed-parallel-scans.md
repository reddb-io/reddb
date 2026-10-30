---
"@reddb-io/cli": patch
---

Filter and project parallel table scans from borrowed entities instead of cloning full matching rows before materialization. This avoids copying unselected binary payloads while preserving snapshot visibility, segment pruning, and scan ordering.
