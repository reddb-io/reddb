# Rotating scoped backup credentials

Build the engine with `backend-s3` and provide `curl` in its runtime image.
Set `REDDB_BACKUP_S3_ENDPOINT`, `REDDB_BACKUP_S3_BUCKET`,
`REDDB_BACKUP_S3_PREFIX` and `REDDB_BACKUP_S3_CREDENTIALS_FILE` (absolute path).
File mode replaces both static key environment variables; mixing them is an
error. Static configuration remains compatible when file mode is absent.

The file is one JSON object, up to 32 KiB, with this schema:

```json
{
  "schema_version": 1,
  "bucket": "synthetic-staging-bucket",
  "prefix": "organization/database/",
  "access_key_id": "temporary-access-key-id",
  "secret_access_key": "temporary-secret-access-key",
  "session_token": "temporary-session-token",
  "expires_at_unix_seconds": 1900000000
}
```

Use an HTTPS endpoint and an exact nonempty prefix ending in `/`. Bucket and
prefix must match the immutable backend configuration. Secret fields must be
nonempty printable ASCII with no whitespace. Credentials expiring within 30
seconds are refused before sending a request; malformed, missing, oversized or
out-of-scope files fail without using static keys or printing file contents.
Every request reopens the file and signs `x-amz-security-token` with the current
access key/secret. Existing backend instances observe atomic file replacement.

The trusted credential issuer must enforce the bucket/prefix and allowed actions
at the provider. The JSON scope check prevents accidental delivery of another
bank's credential file; it cannot restrict an overprivileged credential remotely.
Engines must never receive the parent key. Permit only the backup actions actually
required (including prefix-scoped listing for manifest discovery), and keep
retention/deletion authority separate if the archiver does not need it.

Mount the Kubernetes Secret directory read-only; `subPath` mounts do not receive
Secret rotations. The issuer must renew before expiry and publish one complete
JSON object atomically. This engine change does not create the issuer, grant
Kubernetes Secret access or qualify a backup schedule/PITR restoration. Validate
credential expiry/rotation alerts, provider isolation and independent restore of
the database's full physical layout before enabling customer backup.

An opt-in live test exercises actual S3 upload, download, HEAD and scoped listing
across rotation with one backend instance. It requires a newly minted pair of
single-prefix credentials and only accepts the staging qualification bucket:

```sh
REDDB_S3_ROTATION_FIXTURE=/absolute/private-fixture.json \
  cargo test -p reddb-io-server --features backend-s3 \
  --test s3_rotating_credentials_live -- --ignored
```

The fixture contains `endpoint`, `bucket`, `prefix`, `first_credentials_file` and
`second_credentials_file`. The test writes two synthetic objects in that unique
prefix and preserves them for independent review; it removes only its local
temporary credential file.
