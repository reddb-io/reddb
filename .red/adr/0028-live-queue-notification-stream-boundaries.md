# Live Queue, Notification, And Stream Boundaries

Status: accepted

RedDB will not collapse Honker-style live delivery, ephemeral pub/sub, and durable streams into one overloaded queue primitive. `Live queue wait` extends existing queue delivery and preserves ACK/NACK, DLQ, tenant scope, and authorization; delayed messages and retry/backoff also remain queue behavior. `Ephemeral notification` is a separate non-durable signal with tenant-scoped channels, while `Durable stream` is a separate Collection model for append-only logs with per-consumer offsets.

This split keeps the queue lifecycle from absorbing incompatible semantics. A queue read creates pending delivery state; a stream read advances or records an offset; a notification has no replay state at all. Treating these as separate primitives costs more surface area, but avoids hidden mode switches and lets transports adapt each contract without becoming the source of truth.
