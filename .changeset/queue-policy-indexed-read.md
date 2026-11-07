---
"@reddb-io/cli": patch
---

Read queue lifecycle policy through its existing metadata index instead of scanning the full catalog on delivery and ACK. Avoid duplicate configuration reads in batch ACK and preserve ALTER visibility and cold-boot recovery.
