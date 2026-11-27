---
"@reddb-io/cli": patch
---

Retention of the control-event ledger no longer walks the whole ledger over and over: a pass reads the events on disk in the order they were written and ends at the first one still inside the window. A full pass still runs after a start and every six hours. A collection that a checkpoint has to publish again in full now gets its pending COUNT projection in the same pass, instead of being published in full at every checkpoint until a background build caught up.
