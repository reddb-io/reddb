---
"@reddb-io/cli": patch
---

Update rustls-webpki to 0.103.16 and webpki-roots to 1.0.10 across the workspace,
Python driver and Node addon lockfiles. Reject distrusted emSign intermediate
keys and remove the affected bundled root, addressing RUSTSEC-2026-0382 and
RUSTSEC-2026-0381. Connections using affected certificate chains may now fail
verification; replace the chain rather than disabling verification.
