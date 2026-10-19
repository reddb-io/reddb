---
"@reddb-io/cli": patch
---

Keep catalog and health observations from cloning collection payloads or rebuilding
native search artifacts. Build native artifacts once per physical publication,
and report health's metadata validation scope explicitly. Preserve descriptor
consistency checks and leave full artifact decoding to explicit inspection.

Honor CREATE INDEX IF NOT EXISTS before rebuilding an existing index, and fail
opening a paged store when a collection root or entity payload cannot be decoded
instead of silently returning a partial database.
