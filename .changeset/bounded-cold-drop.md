---
"@reddb-io/cli": patch
---

Keep cold collection drops bounded to their native registry and targeted
physical metadata publication. Avoid fingerprinting append-heavy internal
collection history before acknowledging the drop, and preserve the committed
absence plus unrelated collections across restart.
