---
"@reddb-io/cli": patch
---

Reject main-file-only remote backups of existing paged stores even when they are opened with embedded options. Those stores also require physical metadata sidecars; restoring only the main file can lose declared collection models. Require the existing complete, fenced physical-directory backup path rather than advertising an incomplete snapshot as successful. Self-contained embedded backups remain supported.
