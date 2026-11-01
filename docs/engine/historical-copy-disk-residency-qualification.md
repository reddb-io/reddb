# Historical native-copy qualification

The ignored library test
`storage::unified::store::historical_copy_qualification::disk_resident_historical_copy_preserves_every_exported_entity`
opens the native disk-residency implementation against an independently owned
copy and compares every exported physical entity with an external fingerprint.
It then checkpoints, reopens the prepared generation and repeats the comparison.
It does not enable disk residency in the managed runtime factory.

Run it only in an isolated container with an externally verified memory limit
and swap disabled. The Platform diagnostic uses one CPU and 1 GiB total memory;
the test configures 128 MiB of pager slots and 256 MiB of resident overlays.
Those two allowances do not cover all process memory. Record the container's
actual memory counters, OOM events, immutable binary digest and termination.

The copy directory must contain `reddb.rdb` and all its native sidecars and
operational directory. Its marker `.reddb-owned-qualification-copy` must contain
`disk-residency-historical-qualification-v1`. Do not mount the original or a
serving database in the test container: opening and checkpointing the copy can
prepare a newer native format that an ordinary runtime cannot reopen.

The independent reference is the JSON receipt from a quiescent native export:
`complete: true`, `rows`, and a `collections` array whose entries contain `name`,
`count`, `sha256Sum` and `sha256Xor`. Hash each entity's public native DTO using
SHA-256 and canonical compact JSON; sum the digests modulo 2^256 and XOR them
per collection. Include internal physical collections and retained versions.
Keep private data and credentials outside the repository.

With the container limits already enforced, run the prebuilt library test binary:

```sh
REDDB_QUALIFICATION_COPY_DIR=/candidate \
REDDB_QUALIFICATION_REFERENCE=/work/reference.json \
/work/qualifier \
  --exact storage::unified::store::historical_copy_qualification::disk_resident_historical_copy_preserves_every_exported_entity \
  --ignored --nocapture --test-threads=1
```

`initial open_ms` includes preparation of an older native generation.
`prepared open_ms` measures the subsequent native-store open. Neither measures
full server readiness, authentication, secondary-index bootstrap or release-build
performance. The DTO fingerprints also omit some native schema/MVCC headers;
retain the original native backup and the existing snapshot/recovery tests.

A passing result requires both complete scans and both checkpoints to succeed,
all collection names/counts/hashes to match, resident segments to fit their
allowance, no restart and no OOM. This is one storage qualification component;
it does not establish the full process-wide allocation policy, a 24-hour soak,
the write workload, all query/transport paths or safe fleet deployment/downscale.
