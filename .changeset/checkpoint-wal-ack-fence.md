---
"@reddb-io/cli": patch
---

Fence WAL offset reset against outstanding durable commit acknowledgements and
flusher state publication. A checkpoint could previously discard an enqueued
record or reset its offsets before a waiting writer observed durability, leaving
that writer waiting for an offset in the previous WAL. Allow the flusher to drain
existing commit leases while reset is waiting, and prevent stale flush results
from being published after reset. The WAL format is unchanged.
