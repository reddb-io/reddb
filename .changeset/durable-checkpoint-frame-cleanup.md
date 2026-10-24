---
"@reddb-io/cli": patch
---

Reduce checkpoint write amplification by clearing the double-write recovery zone once after all frames in a protected batch. Every frame retains its staging and home-page durability barriers; interrupted frame replacement remains recoverable without changing the file format.
