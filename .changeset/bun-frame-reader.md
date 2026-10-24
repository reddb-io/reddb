---
"@reddb-io/client-bun": patch
---

Accumulate fragmented RedWire input without repeated buffer concatenation and
copy completed payloads directly into owned result buffers. Release consumed
receive storage and reject pending/future queries on terminal framing, socket or
connection-close failures, while preserving per-query engine error recovery.
