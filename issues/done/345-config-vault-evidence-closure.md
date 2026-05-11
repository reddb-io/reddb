# Config/Vault evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/345

Labels: needs-triage

GitHub issue number: #345

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Verify Config/Vault sealed storage, unseal, redaction, audit, policy, protected system collections, watch/list/tags, and domain-separated API behavior. Record supersession by newer domain API issues where applicable.

Covers: #317, #318, #319, #321

User stories covered: 25, 26, 27

## Acceptance criteria

- [ ] Vault sealed storage and unseal behavior have current test evidence across read/write paths.
- [ ] Vault redaction, audit, and policy behavior are evidenced through public operations.
- [ ] red.config and red.vault system collections are protected and observable as expected.
- [ ] Config/Vault WATCH, LIST, and TAGS are verified or split into missing behavior follow-ups.
- [ ] The evidence report no longer marks #317, #318, #319, or #321 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)
