# Secret Inventory & Operations

This page is the operator-facing index of every credential RedDB
touches. For each secret it answers four questions: where it lives in
your stack, who reads it, how to rotate it, and what fails if it
leaks.

Pair this with:

- [`docs/security/vault.md`](../security/vault.md) — vault internals and bootstrap.
- [`docs/security/tokens.md`](../security/tokens.md) — auth method reference.
- [`docs/security/encryption.md`](../security/encryption.md) — at-rest posture.
- [`docs/operations/runbook.md`](runbook.md) — day-2 operations.

---

## 1. Inventory

| Secret                       | Lives in                              | Consumed by                                  | Rotation cost            | Blast radius if leaked |
|:-----------------------------|:--------------------------------------|:---------------------------------------------|:-------------------------|:-----------------------|
| `REDDB_CERTIFICATE`          | Cloud secret manager + offline copy   | RedDB process at boot (derives `vault_key`)  | High (re-bootstrap)      | Full read of vault: users, SCRAM, API keys, `red.secret.*`, `Value::Secret` columns |
| `REDDB_VAULT_KEY` (legacy)   | Same                                  | Same — fallback when no certificate          | High (re-bootstrap)      | Same as above          |
| `REDDB_USERNAME` / `_PASSWORD` | Cloud secret manager (bootstrap only) | Auto-bootstrap path on a fresh DB            | Low — bootstrap is one-time | Initial admin account  |
| `RED_ADMIN_TOKEN`            | Cloud secret manager                  | Operators + automation calling `/admin/*`    | Low (revoke + re-issue)  | Full admin access to one DB |
| API keys (`rdb_k_*`)         | Application secret stores             | Application code calling RedDB               | Low (revoke + re-issue)  | Scoped to the user/role on the key |
| Session tokens (`rdb_s_*`)   | Browser / app memory                  | Interactive logins                            | Trivial (logout)         | Until expiration       |
| SCRAM verifiers              | Inside the vault                      | RedWire + PG wire SCRAM auth                 | Set new password         | Same as user password  |
| OAuth/OIDC issuer keys       | Your IdP (Auth0, Cognito, Keycloak, …) | RedDB's `JwtVerifier`                       | IdP-managed              | All federated logins   |
| mTLS server cert + key       | Cert manager / cloud KMS              | TLS listener (HTTPS, RedWire-TLS, gRPC-TLS)  | Medium (cert-manager auto-rotates) | Server identity     |
| mTLS client CA               | Same                                  | Server validates incoming client certs       | Medium                   | Authentication bypass for that mesh |
| HMAC shared secret           | Inside the vault (per API key)        | HMAC-signed request validation               | Low (revoke API key)     | Replay-protected forgery for that key |
| `RED_S3_ACCESS_KEY` / `_SECRET_KEY` | Cloud secret manager           | S3 / R2 / DO Spaces / GCS backend            | Low (rotate IAM)         | Full read/write on the backup bucket |
| `RED_TURSO_TOKEN`            | Cloud secret manager                  | Turso backend                                | Low (rotate token)       | Backup tier integrity  |
| `RED_D1_TOKEN`               | Cloud secret manager                  | Cloudflare D1 backend                        | Low (rotate token)       | Backup tier integrity  |
| `RED_BACKEND_HTTP_AUTH`      | Cloud secret manager                  | Generic HTTP backend                         | Low                      | Same — backup tier     |
| Pager / data-volume encryption keys (LUKS, KMS, EBS, EFS) | Cloud KMS / LUKS keystore | Filesystem layer below RedDB | KMS-managed              | Full read of `.rdb` file (vault still encrypted within it) |
| AI provider keys (`red.ai.*`) | Inside `red_config` or `red.secret.*` | AI providers via `EMBED`, `ASK`             | Low                      | Provider quota theft   |

`Value::Secret` columns and `Value::Password` columns are **derived
secrets** — keyed by `red.secret.aes_key` inside the vault. They share
the rotation lifecycle of the vault itself.

---

## 2. Storage layer

Where each class of secret should live. Match the column to the secret
type from [section 1](#1-inventory).

| Storage option              | Suitable for                                | Avoid for                                           |
|:----------------------------|:--------------------------------------------|:----------------------------------------------------|
| **Cloud KMS / Secret Manager** (AWS Secrets Manager, GCP Secret Manager, Azure Key Vault) | Everything in section 1 except short-lived session tokens | Single-developer dev environments — overhead not justified |
| **HashiCorp Vault**         | Everything; especially good for short-lived dynamic credentials and PKI | Edge nodes without network access to a Vault cluster |
| **Docker secrets** (Swarm or `compose secrets:`) | `REDDB_CERTIFICATE`, `RED_ADMIN_TOKEN`, backend creds in single-host deployments | Multi-host orchestration without an external secret store |
| **Kubernetes Secrets**      | Everything when paired with EncryptionConfiguration at the API server | Clusters without etcd-encryption-at-rest configured |
| **systemd `LoadCredential`** | bare-metal / VM deployments with systemd-managed secrets | Containerized workflows                            |
| **`.env` files**            | **Local dev only.** Add to `.gitignore`. Never commit. | Anything that touches a deployed environment        |
| **CI/CD secret store** (GitHub Actions secrets, GitLab CI variables, Buildkite secrets) | Build-time deploy keys, image-pull tokens | Long-lived production runtime secrets — prefer workload identity |
| **Application memory / config maps** | Public configuration only                     | Anything sensitive — config maps are unencrypted in etcd |

> [!IMPORTANT]
> A secret manager that is itself authenticated by long-lived static
> credentials is no better than the static credentials themselves. Use
> workload identity (IAM roles for service accounts on EKS/GKE/AKS,
> IRSA on AWS, Workload Identity Federation on GCP, Managed Identities
> on Azure) wherever the platform supports it.

---

## 3. Rotation matrix

| Secret                      | Recommended cadence | Procedure                                                             | Rollback                                              |
|:----------------------------|:--------------------|:----------------------------------------------------------------------|:------------------------------------------------------|
| `REDDB_CERTIFICATE`         | Quarterly + on suspected leak | See [vault.md §12](../security/vault.md#rotation-procedure): bootstrap fresh DB, restore from logical backup, cut over | Restore old volume; the old cert still works on it    |
| Bootstrap admin password    | Once (you set a strong one) + on personnel change | `red auth set-password admin --password <new>` then update secret manager | Re-set previous password from your password vault    |
| `RED_ADMIN_TOKEN`           | Monthly             | Mint new key → update secret manager → `kill -HUP <pid>` → revoke old via `/auth/api-keys/<id>` | Re-fetch old key from secret manager history (if your store retains versions) |
| API keys                    | Per app's policy (90 days typical) | `red auth create-api-key <user>` → update app config → revoke old key | Re-mint and update the app                            |
| SCRAM password (per user)   | Per identity policy | `red auth set-scram <user> --password <new>`                          | Re-set old password — but old SCRAM verifier is gone after a new one is set |
| mTLS server cert            | 90 days (Let's Encrypt-style) | cert-manager auto-renew + reload via SIGHUP (mTLS files are `*_FILE`) | Restore previous cert from cert-manager history       |
| mTLS client CA              | Annually + on private-key compromise | Generate new CA → bridge period serving both → cut over | Keep old CA in trust bundle for the bridge window     |
| OAuth/OIDC keys             | IdP-managed         | IdP rotates JWKS → RedDB picks up via JWKS URL refresh                 | IdP rolls back to previous key                         |
| `RED_S3_ACCESS_KEY` / `_SECRET_KEY` | 90 days     | Create new IAM key → update secret manager → SIGHUP → delete old IAM key | Recreate the previous IAM key                          |
| `RED_TURSO_TOKEN`           | 90 days             | `turso db tokens create` → update secret manager → SIGHUP → revoke old | `turso db tokens create` for old DB                    |
| `RED_D1_TOKEN`              | 90 days             | Cloudflare API → rotate → update → SIGHUP                              | Same                                                   |
| LUKS / KMS data-volume keys | KMS-managed         | KMS rotates underlying key automatically; no app action required       | KMS rolls back to previous version                     |
| AI provider keys            | Per provider policy | Provider portal → rotate → update via `/ai/credentials` POST → SIGHUP optional | Provider re-issue old key                          |

### What "SIGHUP" buys you

For every `*_FILE` companion variable (see
[vault.md §5](../security/vault.md#5-restart--unseal-precedence-and-_file-vars)),
sending `SIGHUP` to the process re-reads the file in place. This means:

- Update the secret in your store (cloud SM, K8s Secret, file).
- Wait for the orchestrator to refresh the mounted file (kubelet:
  ~60s; Vault Agent: change-mode signal; Docker secrets: file is
  always live since it's a tmpfs mount).
- `kill -HUP $(pgrep red)` (or use a sidecar that signals on file
  change).
- The new value is live without a process restart, no connections
  dropped.

The certificate is an exception: it sealed the vault at boot and
cannot be hot-swapped. Cert rotation always requires a restart and
is rare by design.

### What if rotation fails?

| Failure mode                    | Detection                              | Mitigation                                                  |
|:--------------------------------|:---------------------------------------|:------------------------------------------------------------|
| New API key not propagated to all clients | App returns 401 after rotation | Keep both keys live during a bridge window (24-72h); revoke old after observation |
| SIGHUP picked up partial / corrupt secret file | Process logs `secret_file_read_error` | Restore the previous file, SIGHUP again; investigate the writer |
| New mTLS cert wrong CN          | TLS handshake fails                    | Roll back the cert in cert-manager; investigate the issuance |
| Cert rotation mid-restart, replicas can't reconnect | Replicas log `vault_open_error` | Push the new cert to replicas first; restart primary last  |

---

## 4. Incident response — `cert leaked`

The certificate is the highest-impact secret. Treat any suspected leak
as a P1 incident.

### Immediate actions (T+0 to T+30 min)

1. **Isolate.** Block public network access to the affected RedDB
   instance. The leaked cert still works against any reachable
   replica.
2. **Notify.** Alert on-call security + the data owner. Document the
   leak vector and time of detection.
3. **Verify backups.** Confirm a logical backup exists less than 24h
   old:

   ```bash
   curl http://primary:8080/admin/backups \
     -H "Authorization: Bearer $RED_ADMIN_TOKEN" | jq '.[] | {id, taken_at}'
   ```

   If none exists, **take one now** — the leaked cert is still valid
   on the running instance, so a backup is recoverable.

### Plan and execute the rotation (T+30 min to T+4 hours)

4. **Bootstrap a fresh DB** with a new certificate
   ([vault.md §4](../security/vault.md#4-bootstrap-cli--http)). Ship
   the new cert to the secret manager.
5. **Restore user data** from the logical backup:

   ```bash
   curl -X POST http://new:8080/admin/restore \
     -H "Authorization: Bearer $NEW_ADMIN_TOKEN" \
     -d '{"snapshot_id":"<latest>","skip_vault":true}'
   ```

   `skip_vault: true` is critical — restoring vault state would bring
   back the leaked credentials.
6. **Re-issue admin and API keys.** None of the old keys carry over.
7. **Re-set every `red.secret.*` value** from your application's
   secret store.
8. **Cut over.** DNS or load-balancer flip; old instance offline.

### Decommission and audit (T+4 hours to T+24 hours)

9. **Wipe the old volume.** `dd if=/dev/zero` or cloud-provider
   secure-erase. The leaked cert can decrypt it forever.
10. **Rotate any secret that lived inside the old vault.** Treat them
    as exposed: third-party API keys, OAuth client secrets, anything
    in `red.secret.*`, anything stored in `Value::Secret` columns.
11. **Audit log review.** Cross-reference your secret-manager access
    log against your operations log. Identify the leak vector.
12. **Post-mortem.** Document the incident, close the leak vector,
    update this runbook if any procedure was missing or wrong.

### What you cannot do

- **Roll forward without a backup.** No backup = data loss. Period.
- **Keep the old cert "for forensics"** while running the new one.
  The old cert is now an attacker primitive; treat it as such.
- **Trust replicas that booted with the leaked cert.** Decommission
  every replica that ever held the cert; bootstrap fresh ones.

---

## 5. Disaster recovery

The cert backup is independent of the data backup, but both must exist
for a clean restore.

### Backup cadence

| Artifact                       | Recommended cadence                          | Storage                                              |
|:-------------------------------|:---------------------------------------------|:-----------------------------------------------------|
| `.rdb` logical backup (`/admin/backup`) | Every 6h for prod, every 24h for staging | S3 + cross-region replica + lifecycle to Glacier  |
| WAL archive                    | Continuous (configured by `RED_WAL_ARCHIVE_*`) | Same                                              |
| `REDDB_CERTIFICATE` (offline)  | Once + on every rotation                     | Tamper-evident envelope in physical safe + cloud SM   |
| `REDDB_CERTIFICATE` (cloud SM) | Always live                                  | AWS SM / GCP SM / Azure Key Vault, multi-region replicated |
| TLS cert + key                 | Per-rotation                                 | cert-manager + KMS                                   |
| Backend creds                  | Per-rotation                                 | Cloud SM                                             |

### Quarterly DR drill

Test restore in staging every quarter. The drill validates:

1. The cert backup is readable (you can decode the hex).
2. The data backup is restorable into a fresh instance.
3. The application reconnects with a new admin token.
4. CDC consumers resume from the correct LSN after restore.

```bash
# Simplified drill script — full version in scripts/dr-drill.sh
make dr-drill PROFILE=staging
```

If the drill fails, that's a P2 finding — fix the gap, re-run the
drill, document the result.

### What fails without a cert backup

| Scenario                          | Outcome                                                         |
|:----------------------------------|:----------------------------------------------------------------|
| Live primary lost, cert in cloud SM | Restore data backup into fresh instance + apply old cert → full recovery |
| Live primary lost, cert lost       | Restore data backup into fresh instance with NEW cert + `skip_vault: true` → user data only, lose every credential and `red.secret.*` value |
| Live primary running, cert lost    | DB still works (running process holds derived `vault_key`); next restart fails. Take a logical backup, bootstrap fresh, swap.  |

---

## 6. Audit and compliance

### What to log

- Every `GetSecretValue`-equivalent call on the cert (cloud SM emits
  this natively).
- Every `/admin/*` call on RedDB (audit logged automatically; surface
  via `GET /admin/audit`).
- Every change to a Kubernetes Secret containing the cert (audit
  webhook on the API server).
- Every CI run that builds an image that touches secret-mounted
  paths.

### Cross-references for compliance frameworks

| Framework | Relevant control                                                  | How RedDB satisfies it                                  |
|:----------|:------------------------------------------------------------------|:--------------------------------------------------------|
| SOC 2 — CC6.1 (Logical Access) | Restrict access to sensitive data | RBAC + RLS + vault encryption                          |
| SOC 2 — CC6.6 (Encryption in transit) | TLS for all customer data | TLS available on all transports                       |
| SOC 2 — CC6.7 (Encryption at rest) | Encrypt sensitive data at rest | Vault encrypts auth state + `red.secret.*`; infra encrypts pages |
| ISO 27001 — A.9 (Access control) | Privileged access management | Segregation of duties pattern in [vault.md §12](../security/vault.md#segregation-of-duties) |
| GDPR — Art. 32 (Security of processing) | State of the art                 | AES-256-GCM, Argon2id, RFC 5802 SCRAM, mTLS, OIDC      |
| PCI-DSS — Req. 3 (Stored cardholder data) | Render PAN unreadable          | `Value::Secret` columns + RLS + audit log              |

### Segregation of duties — concrete pattern

Three roles, no overlap (except the break-glass account):

| Role               | Cloud SM read on cert | RedDB admin token | Restart pod  | DB superuser ssh |
|:-------------------|:---------------------:|:-----------------:|:------------:|:----------------:|
| Application developer |          ❌            |        ❌          |       ❌       |        ❌         |
| SRE / on-call      |          ❌            |        ✅          |       ✅       |        ❌         |
| Secrets custodian  |          ✅            |        ❌          |       ❌       |        ❌         |
| DB administrator   |          ❌            |        ✅          |       ✅       |        ✅         |
| Break-glass        |          ✅            |        ✅          |       ✅       |        ✅         | (alert + dual-control + auto-disable after 1h) |

Document this matrix in your access registry. Review at every joiner
/ leaver / role change.

---

## 7. Anti-patterns

These are common mistakes. Audit your stack against this list.

| Anti-pattern                                                     | Why it's bad                                          | Replace with                                              |
|:-----------------------------------------------------------------|:------------------------------------------------------|:----------------------------------------------------------|
| Cert in `.env` checked into git, even private repo               | Repo history is forever. Anyone with `git log` access has the cert. | Cloud secret manager + workload identity                  |
| Cert in Helm `values.yaml`                                       | Same as above when checked in; same as `kubectl apply -f` when not | `existingSecret:` reference in values + ESO sync          |
| Long-lived cloud creds (static AWS access key) granting access to the cert | Static creds = persistent attack surface | IAM role + workload identity                              |
| Same cert across dev / staging / prod                            | Lateral movement: dev compromise = prod compromise    | Per-env certs + per-env secret manager paths             |
| Cert printed to CI build log                                     | Logs ship to S3 / Splunk forever; many people have read access | Mask the cert; capture once via stdout-redirect to secret manager |
| Cert sent over Slack / email / pager                             | Persistent in message history; often forwarded        | Direct write into secret manager; share the *path*, not the value |
| Cert unrotated for >12 months                                    | Stale; high probability someone with old access still has it | Quarterly rotation                                        |
| No offline backup of the cert                                    | Single point of failure (cloud SM outage = disaster)  | Tamper-evident envelope in physical safe                  |
| Cert backup stored next to data backup                           | One compromise gets both                              | Separate the backup tiers — different account, different region |
| Operator who restarts the pod can also read the cert             | Single compromise = full data exfil                   | Segregation of duties (see section 6)                     |
| `chmod 644 cert.pem`                                             | World-readable; any local process reads the cert      | `chmod 400 cert.pem`; tmpfs mount with mode `0400`        |
| Logging successful auth payloads with the bearer token           | Tokens leak through log aggregation                   | Log the user identity, not the token; redact `Authorization` header |
| Long-lived bearer token used as if it were a password            | One compromise = persistent access                    | Short-lived OAuth tokens + refresh; or rotate API keys monthly |

---

## 8. Cross-references

- [`docs/security/vault.md`](../security/vault.md) — vault internals, bootstrap, key hierarchy, recovery.
- [`docs/security/encryption.md`](../security/encryption.md) — at-rest posture, infrastructure encryption.
- [`docs/security/tokens.md`](../security/tokens.md) — auth method reference (API keys, SCRAM, OAuth, HMAC, mTLS).
- [`docs/security/overview.md`](../security/overview.md) — RBAC, RLS, multi-tenancy.
- [`docs/getting-started/docker.md`](../getting-started/docker.md) — production-secure Docker patterns.
- [`docs/operations/runbook.md`](runbook.md) — day-2 operations.
- [`charts/reddb/README.md`](../../charts/reddb/README.md) — Helm chart secrets surface.
