# Historical native-copy qualification

The ignored library test
`presentation::entity_json::historical_copy_qualification::disk_resident_historical_copy_preserves_every_exported_entity`
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
For this comparison the disk view uses the exact ordinary-recovery row-layout
projection: eligible named rows become positional, internal named collections
remain named, schema diversity stays bounded, and conflicting explicit positions
are preserved. Raw named/positional encoding is not itself a value invariant.
Keep private data and credentials outside the repository.

Freeze the exporter before choosing the final reference cutoff. Authentication
can append internal login-audit rows even when application data is read-only.
Retain the original receipt; reconcile only individually identified test-owned
events by their IDs, timestamps and hashes. Extend the original count/sum/XOR
with those exact events instead of ignoring an internal collection or allowing
an arbitrary count tolerance. The complete frozen comparison must still pass.

With the container limits already enforced, run the prebuilt library test binary:

```sh
REDDB_QUALIFICATION_COPY_DIR=/candidate \
REDDB_QUALIFICATION_REFERENCE=/work/reference.json \
/work/qualifier \
  --exact presentation::entity_json::historical_copy_qualification::disk_resident_historical_copy_preserves_every_exported_entity \
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

The separate ignored test
`presentation::entity_json::historical_copy_qualification::complete_runtime_historical_copy_preserves_sql_results`
opens the real library runtime through a thread-local, test-only factory scope.
It requires the same owned-copy marker plus `REDDB_QUALIFICATION_SQL_REFERENCE`,
a private JSON array of the ten diagnostic cases: `name`, `sql`, and `rows`
independently captured from the source HTTP API. It compares all ten results,
checkpoints, reopens, and repeats with a 768-MiB engine budget. Keep the external
1-GiB process limit and capture actual termination, memory peaks and OOM events.
The runtime can publish internal configuration during boot; this probe changes
the independent copy's internal records, so retain earlier native fingerprints
and their exact cutoff rather than silently replacing them.

This probe measures runtime construction and SQL correctness. It does not start
an HTTP/auth listener, qualify every restored secondary index, control filesystem
cache temperature, establish optimized performance or enable the production
factory. The ordinary suite also exercises a small persistent runtime through
this exact factory, committed CRUD, a checkpoint and a second cold reopen.
