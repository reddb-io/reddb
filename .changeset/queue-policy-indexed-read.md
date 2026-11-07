---
"@reddb-io/cli": patch
---

Read queue lifecycle policy through its existing metadata index instead of scanning the full catalog on delivery and ACK. Avoid duplicate configuration reads in batch ACK. Keep ALTER changes visible and fix disk-backed FANOUT, delivery deadline and DLQ policy falling back to defaults after reopen.
