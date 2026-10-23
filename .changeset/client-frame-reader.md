---
"@reddb-io/client": patch
---

Accumulate fragmented RedWire frames without repeatedly copying incomplete payloads,
release consumed receive buffers, and reject pending reads when a connection ends
mid-frame or framing fails. Wire bytes and public APIs remain unchanged.
