# Docker — Quickstart and Production

RedDB ships as a single static binary in a minimal container image. This
page covers two paths: a one-liner for local development, and a
production-secure pattern using Docker secrets + the encrypted vault.

> [!NOTE]
> Prebuilt images are published to GitHub Container Registry under
> `ghcr.io/reddb-io/*`. If `docker pull ghcr.io/reddb-io/reddb:latest`
> returns `unauthorized`, authenticate to GHCR first:
>
> ```bash
> echo "$GITHUB_TOKEN" | docker login ghcr.io -u "$GITHUB_USER" --password-stdin
> ```
>
> The Compose files in `examples/` use `build:` from your checkout and do not
> require GHCR access.

For deployment to Kubernetes, ECS, App Runner, Cloud Run, or any other
orchestrator, see the platform-specific manifests in [`examples/`](../../examples/)
and the secret-management patterns in
[`docs/security/vault.md`](../security/vault.md).

---

## 1. Quickstart (no auth, dev only)

Using the prebuilt GHCR image:

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

If you do not have GHCR access, build locally instead:

```bash
docker build -t reddb .
docker run --rm -p 5050:5050 -p 55055:55055 -p 5000:5000 reddb
```

That's it. RedDB binds RedWire on `0.0.0.0:5050`, gRPC on
`0.0.0.0:55055`, HTTP/Web/health on `0.0.0.0:5000`, and starts an
in-container ephemeral database.

Open the admin health endpoint:

```bash
curl http://127.0.0.1:5000/admin/health
# {"ok":true,"version":"x.y.z"}
```

Run a query:

```bash
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT 1"}'
```

> [!WARNING]
> This container runs without authentication. Anyone who can reach the
> exposed ports can read, write, and delete data. Do **not** expose
> ports 5050, 55055, or 5000 to anything beyond your laptop.

To persist data across container restarts, mount a volume:

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -v reddb-dev-data:/data \
  ghcr.io/reddb-io/reddb:latest
```

---

## 2. Production-secure (vault + Docker secrets)

The production pattern has four moving parts:

1. **Bootstrap once** in a one-off container. Capture the certificate.
2. **Store the certificate** in your secret manager.
3. **Materialize the cert** as a Docker secret on a tmpfs mount.
4. **Run the real container** with `REDDB_CERTIFICATE_FILE` pointing at
   that mount.

### Step 1 — Bootstrap

```bash
mkdir -p ./reddb-data ./secrets

# Run bootstrap in a one-off container; capture stdout
docker run --rm \
  -v "$(pwd)/reddb-data:/data" \
  ghcr.io/reddb-io/reddb:latest \
  bootstrap \
    --path /data/data.rdb \
    --username admin \
    --password "$(openssl rand -base64 24)" \
    --print-certificate \
  | tee /tmp/reddb-bootstrap.log

# Extract the cert
CERT=$(grep '^certificate:' /tmp/reddb-bootstrap.log | awk '{print $2}')

echo "$CERT" > ./secrets/reddb_certificate.txt
chmod 0400 ./secrets/reddb_certificate.txt

shred -u /tmp/reddb-bootstrap.log
```

The cert is 64 hex chars. The bootstrap also creates `admin` and prints
its initial API key — capture both.

### Step 2 — Store the certificate

This example uses a local file for the demo. **In real production, push
the cert into your cloud secret manager** (AWS Secrets Manager, GCP
Secret Manager, HashiCorp Vault, etc.) and skip the local file. See
[`docs/security/vault.md` §10](../security/vault.md#10-cloud-native-secret-managers).

```bash
# Local-file pattern (dev/staging only)
ls -l ./secrets/reddb_certificate.txt
# -r-------- 1 you you 65 ...
```

### Step 3 + 4 — Run with `docker-compose.vault.yml`

```yaml
# examples/docker-compose.vault.yml
services:
  reddb:
    image: ghcr.io/reddb-io/reddb:latest
    ports:
      - "5050:5050"
      - "55055:55055"
      - "5000:5000"
    volumes:
      - reddb-data:/data
    environment:
      REDDB_TOPOLOGY: standalone
      REDDB_NODE_ROLE: standalone
      REDDB_STORAGE_PRESET: embedded
      REDDB_STORAGE_PROFILE: embedded
      REDDB_STORAGE_PACKAGING: single-file
      REDDB_REPLICA_COUNT: "0"
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate
    secrets:
      - reddb_certificate
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:5000"]
      interval: 10s
      timeout: 5s
      retries: 3

secrets:
  reddb_certificate:
    file: ./secrets/reddb_certificate.txt

volumes:
  reddb-data:
```

```bash
docker compose -f examples/docker-compose.vault.yml up -d
docker compose -f examples/docker-compose.vault.yml logs -f
```

The compose file mounts `./secrets/reddb_certificate.txt` at
`/run/secrets/reddb_certificate` on a Docker-managed tmpfs (RAM-backed,
mode `0400`). The cert never lands on disk inside the container, never
appears in `docker inspect`, and never gets baked into the image.

Verify the vault is unsealed:

```bash
curl -s http://127.0.0.1:5000/admin/status \
  -H "Authorization: Bearer $RED_ADMIN_TOKEN" | jq '.vault'
# {"state":"unsealed","backend":"page-2-3","cipher":"aes-256-gcm"}
```

---

## 3. `docker-compose.vault.yml` walkthrough

Reading the example file line by line.

```yaml
services:
  reddb:
    image: ghcr.io/reddb-io/reddb:latest
```

Use the official multi-arch image. Pin to a specific tag (`:1.2.3`) in
production rather than `:latest`.

```yaml
    ports:
      - "5000:5000"     # HTTP / admin / metrics
      - "5050:5050"     # RedWire binary protocol
      - "55055:55055"     # gRPC API / replica control plane
```

Three standard container transports. PG wire (`--pg-bind`) and a separate
admin port (via `RED_ADMIN_BIND`) are off by default; turn them on if you need
them.

```yaml
    volumes:
      - reddb-data:/data
```

Named volume — the `.rdb` file lives at `/data/data.rdb`. Loss of this
volume = total data loss unless you have backups configured.

```yaml
    environment:
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate
```

The `*_FILE` form (preferred over inline `REDDB_CERTIFICATE=...`)
points at the secret mount. The binary expands supported `*_FILE` env vars
during boot, so the secret value is read from the mounted file instead of
being baked into the image.

```yaml
    secrets:
      - reddb_certificate
```

Names the secret bindings. Each named secret in this list is mounted at
`/run/secrets/<name>`.

```yaml
    environment:
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate
```

`REDDB_VAULT=true` enables the encrypted auth vault. Without it, the default
container runs in local unauthenticated mode.

```yaml
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:5000"]
      interval: 10s
      timeout: 5s
      retries: 3
```

`red health` exits non-zero when the HTTP health endpoint is unhealthy.
Compose marks the container `unhealthy` after three consecutive failures.
Adjust `interval` based on your platform's pull-image latency.

```yaml
secrets:
  reddb_certificate:
    file: ./secrets/reddb_certificate.txt
```

Local-file backend. For production with Docker Swarm, use
`external: true` and create the secret with `docker secret create`. For
non-Swarm production, use Kubernetes Secrets (see
[vault.md §9](../security/vault.md#9-kubernetes)).

```yaml
volumes:
  reddb-data:
```

A named volume. For backups, mount a host path or a cloud-volume CSI
driver instead.

---

## 4. Restart and upgrade

### Restart in place

The vault re-seals when the process exits and unseals on the next
start. You don't lose any auth state across restarts; the cert + page
salt + Argon2id derivation are fully deterministic.

```bash
docker compose -f examples/docker-compose.vault.yml restart reddb
```

The container logs report:

```text
vault: opening with REDDB_CERTIFICATE
vault: unsealed (page=2, salt=..., kdf=argon2id m=16384 t=3 p=1)
http: listening on 0.0.0.0:5000
```

### Upgrade to a new RedDB version

1. Pull the new image:

   ```bash
   echo "$GITHUB_TOKEN" | docker login ghcr.io -u "$GITHUB_USER" --password-stdin
   docker compose -f examples/docker-compose.vault.yml pull
   ```

2. (Optional) take a backup first:

   ```bash
   curl -X POST http://127.0.0.1:5000/admin/backup \
     -H "Authorization: Bearer $RED_ADMIN_TOKEN"
   ```

3. Recreate the container:

   ```bash
   docker compose -f examples/docker-compose.vault.yml up -d
   ```

4. Verify health:

   ```bash
   docker compose -f examples/docker-compose.vault.yml exec reddb \
     red doctor --bind 127.0.0.1:5000
   ```

The data file format is forward- and backward-compatible across patch
and minor releases. Major release bumps (e.g. v1 → v2) include a
migration note in the release notes.

### Rotate the admin token without a restart

```bash
# Mint a new token
NEW_TOKEN=$(curl -X POST http://127.0.0.1:5000/auth/api-keys \
  -H "Authorization: Bearer $OLD_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"username":"admin","name":"rotated","role":"admin"}' \
  | jq -r .key)

# Update the secret file (the tmpfs mount picks up the change)
echo "$NEW_TOKEN" > ./secrets/reddb_admin_token.txt

# Reload secrets in the running process
docker compose -f examples/docker-compose.vault.yml kill -s SIGHUP reddb
```

---

## 5. Multi-replica with shared cert

Replicas need the same cert as the primary — they're opening the same
vault, just on a different volume populated by snapshot restore.

```yaml
# docker-compose.replica-vault.yml
services:
  primary:
    image: ghcr.io/reddb-io/reddb:latest
    ports: [ "5050:5050", "55055:55055", "5000:5000" ]
    volumes: [ primary-data:/data ]
    environment:
      REDDB_TOPOLOGY: primary-replica
      REDDB_NODE_ROLE: primary
      REDDB_STORAGE_PRESET: primary-replica-production-ha
      REDDB_STORAGE_PROFILE: primary-replica
      REDDB_STORAGE_PACKAGING: operational-directory
      REDDB_REPLICA_COUNT: "1"
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate
    secrets: [ reddb_certificate ]

  replica:
    image: ghcr.io/reddb-io/reddb:latest
    ports: [ "5051:5050", "55056:55055", "5001:5000" ]
    volumes: [ replica-data:/data ]
    environment:
      REDDB_TOPOLOGY: primary-replica
      REDDB_NODE_ROLE: replica
      REDDB_STORAGE_PRESET: primary-replica-production-ha
      REDDB_STORAGE_PROFILE: primary-replica
      REDDB_STORAGE_PACKAGING: operational-directory
      REDDB_REPLICA_COUNT: "1"
      REDDB_PRIMARY_ADDR: http://primary:55055
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate
    secrets: [ reddb_certificate ]
    depends_on:
      primary: { condition: service_healthy }

secrets:
  reddb_certificate:
    file: ./secrets/reddb_certificate.txt

volumes:
  primary-data:
  replica-data:
```

Both services mount the same secret. The replica boots with no data,
auto-restores from the primary's snapshot stream, and unseals the vault
with the shared cert.

---

## 6. Local dev shortcut

For development on your laptop, the image default keeps `REDDB_VAULT=false`
and saves you from managing certs. Use this only when:

- You're running on `127.0.0.1` only.
- The data has no privacy implications.
- You're OK with anonymous read/write to your local DB.

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Or, to keep data across restarts:

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -v reddb-dev:/data \
  ghcr.io/reddb-io/reddb:latest
```

The image sets `REDDB_VAULT=false` by default for local bootability. Set
`-e REDDB_VAULT=true` and provide `REDDB_CERTIFICATE_FILE` for vault mode.

---

## 7. Image variants

| Tag                    | Base               | Size (approx) | When to use                                   |
|:-----------------------|:-------------------|:--------------|:----------------------------------------------|
| `:latest` / `:vX.Y.Z`  | `debian:bookworm-slim` | ~80 MB    | Default. Glibc-linked, broad TLS / CA support, easy `apt-get install` for debugging tools. |
| `:vX.Y.Z-musl`         | `scratch` / `gcr.io/distroless/static` | ~20 MB | Static binary (`musl`-linked), no shell, no package manager. Use for hardened deployments. |
| `:vX.Y.Z-alpine`       | `alpine:3.20`      | ~25 MB        | Alpine + apk if you prefer Alpine over Debian. |
| `ghcr.io/reddb-io/reddb:nightly` | `debian-slim` | ~80 MB | Latest `main` build. Do not use in production. |

`:musl` is recommended for production: smaller attack surface, no
shell to exploit, faster cold starts. Use `:debian-slim` if you need
to `docker exec` for debugging.

---

## 8. Healthchecks and signals

### HEALTHCHECK directive

The default image declares:

```dockerfile
HEALTHCHECK --interval=10s --timeout=5s --retries=3 \
  CMD red health --http --bind 127.0.0.1:5000 || exit 1
```

`red health` probes the HTTP health endpoint. Use `red doctor` when you want
the heavier operator check against `/admin/status` and `/metrics`; it applies
operator-tunable thresholds and exits `0|1|2`. See
[`docs/api/cli.md#red-doctor`](../api/cli.md#red-doctor).

### Signal handling

| Signal     | Behavior                                                         |
|:-----------|:-----------------------------------------------------------------|
| `SIGTERM`  | Graceful drain: stop accepting new connections, flush WAL, optionally take a final backup if `RED_BACKUP_ON_SHUTDOWN=true`, exit. Container orchestrators send this on stop. |
| `SIGINT`   | Same as `SIGTERM`.                                               |
| `SIGHUP`   | Reload every `*_FILE` companion env var in place. Used for token rotation. |
| `SIGUSR1`  | Trigger a checkpoint immediately. Useful for forcing a stable state before a snapshot. |
| `SIGKILL`  | Process dies immediately. WAL replay on next start recovers any in-flight commits. |

The default termination grace period in the example compose file is
30 seconds. For workloads with large WAL queues, bump it via
`stop_grace_period: 60s`.

```yaml
services:
  reddb:
    # ...
    stop_grace_period: 60s
    environment:
      RED_BACKUP_ON_SHUTDOWN: "true"
```

---

## 9. Cross-references

- [Vault — encrypted auth & secret storage](../security/vault.md) — full reference for the vault, key hierarchy, bootstrap, and recovery.
- [Secret Inventory & Operations](../operations/secrets.md) — every secret in the stack, rotation matrix, incident response.
- [Encryption at Rest](../security/encryption.md) — at-rest posture; pager vs vault scope.
- [Auth & Security Overview](../security/overview.md) — RBAC, RLS, multi-tenancy.
- [Operator Runbook](../operations/runbook.md) — day-2 operations.
- [Helm Chart README](../../charts/reddb/README.md) — Kubernetes deployment.
- [`examples/` reference manifests](../../examples/) — ECS, App Runner, Cloud Run, Fly Machines, Nomad, Lambda+EFS.
