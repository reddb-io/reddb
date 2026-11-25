---
"@reddb-io/cli": patch
---

A queue read no longer deadlocks the server against a concurrent acknowledgement. Listing queue messages scans the collection under the segment read locks and reads each message's metadata from inside the scan; when an acknowledgement or delete queued for the same segment in between, the metadata read parked behind it while it waited for the scan, and neither proceeded. The acknowledgement holds the commit lock, so every login (which records a control event) hung behind it and readiness stayed degraded until the process was killed. The metadata point read now takes its locks without yielding to a queued writer, which also covers the other scans that read metadata per row (local reads with metadata, vector clustering, serverless snapshots).
