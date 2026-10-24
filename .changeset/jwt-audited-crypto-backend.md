---
"@reddb-io/cli": patch
---

Use the AWS-LC backend for JWT signing and validation, removing the crypto-bigint dependency affected by RUSTSEC-2026-0387 while preserving the existing token API and validation policy.
