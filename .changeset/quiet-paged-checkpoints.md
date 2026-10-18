---
"@reddb-io/cli": patch
---

Reuse unchanged paged collection trees and physical metadata fingerprints during checkpoints. Track segment mutations, rollback visibility and pending vault publication so checkpoint reuse preserves acknowledged data and existing durable publication ordering.
