# Populated WAL-pressure diagnostic — 2026-10-21

Before: `fae39eb6d07c32f8bc734973f10a9349a8e56a45`, executable SHA256
`3337efdfdd686fafca7d9d02fc82f53c18bc589824f391300946256c45ac1adf`.
After: `e0d7757705f93fa4b4e033089c42eaf5ad48cef5`, executable SHA256
`7770fb790f2d32c4099979c37e0dafb590f3ea9d99329a83bcc4265ad486668c`.
Harness: `09de1f54`; later evidence commits have identical Rust source.

Four byte-identical copies of the populated artifact hold 11,000 background
keys plus 1,000 overwrite targets. Each case measures five rounds of 10,000
HTTP PUTs with checked 100-byte values, eight connections and pipeline one.
Server: four CPUs, 2 GiB limit, 1 GiB budget, four workers, 64 handlers, result
cache disabled. Client: two separate CPUs and 1 GiB. Group window: 200 us.
Fixed case order: before-Strict, after-Strict, after-grouped, before-grouped.
No build, strace or perf process ran during timing.

The tables contain medians of five correlated round metrics, not pooled
request percentiles. Ranges are round minima/maxima. This shared-host,
single-process-per-case study has no independent-process confidence interval.

| Mode / binary | Writes/s (range) | p99 ms (range) | p99.9 ms | Worst completion ms |
| --- | ---: | ---: | ---: | ---: |
| Strict before | 138.0 (67.0–193.0) | 328.78 (111.06–1,352.57) | 1,331.33 | 6,243.52 |
| Strict after | 184.7 (117.7–211.0) | 209.50 (78.27–462.81) | 1,208.63 | 6,647.30 |
| Grouped before | 512.6 (146.8–611.1) | 163.67 (130.89–668.93) | 411.73 | 4,972.90 |
| Grouped after | 817.8 (632.7–853.4) | 22.62 (19.60–30.46) | 70.13 | 465.48 |

Grouped throughput increased 59.5% in these medians and p99 fell 86.2%.
Strict throughput increased 33.9% and p99 fell 36.3%, but its throughput ranges
overlap and its worst pause increased. Full-snapshot and synchronous-flush
stalls remain. The fixed order and observed variance prevent a general causal
capacity or worst-case latency claim. These 10,000-operation rounds cannot be
substituted into the earlier 500-operation Redis/KiviDB comparison; no competing
engine was rerun here.

Snapshot SHA256 changed in all five rounds of every case (20/20), confirming
actual checkpoint crossings after both binaries' warmup. Before retained a
64 KiB WAL reservation; after retained 1 MiB. Telemetry runs outside timed
requests. CRC32 of an internally checksummed snapshot can have a constant
residue, so CRC changes are not used as rollover evidence. No request errors
occurred across 200,000 measured writes. Final values were checked after every
round. All four SIGKILL/reopen checks verified every target and background key.

| Case | Server CPU % | Server peak working set MiB | Client CPU % | Client peak working set MiB |
| --- | ---: | ---: | ---: | ---: |
| Strict before | 12.21 | 165.1 | 5.45 | 19.57 |
| Strict after | 14.26 | 159.7 | 6.98 | 18.94 |
| Grouped before | 28.18 | 175.5 | 14.24 | 18.92 |
| Grouped after | 52.97 | 199.9 | 32.89 | 18.53 |

CPU is the median sampled cgroup percentage (100% per core); memory is sampled
working set, not RSS or per-key usage. Raw resource streams and I/O counters
are retained. Python/HTTP overhead and shared storage limit interpretation.

The implementation revalidates stale full results under publication before
checkpointing, adds bounded adaptive headroom, separates empty FIFO fences
from payload batches and fences uncertain checkpoint failures until reopen.
Both existing durability barriers and the file format remain unchanged. It
does not make checkpoints incremental. Extra reservation permits more WAL
replay on reopen; fresh files remain 64 KiB and large indivisible actions keep
their existing contract.

The exact Rust source passed full Linux CI in
[run 39592643015](https://github.com/reddb-io/reddb/actions/runs/39592643015),
including all five new regressions. Native file validation passed 10 grouping
and 29 artifact tests. Large local server unit builds were interrupted by disk
pressure; no final local server-unit pass is claimed. The successful immutable
Linux production build and interrupted unit log are preserved. Details and
red/green evidence are in `validation.json` and `linux-ci-regressions.log`.

`manifest.json` records fixture checksum, image/container identities, limits,
engine identities and harness hashes. `summary.json` records raw metrics and
artifact checksums. Reproduction uses the exact build volume and stopped
fixture described in the manifest; fresh reproductions need new container,
volume and output names. The local capture command was:

```powershell
python bench/results/kv-compare-wal-2026-10-21/run.py --after-revision e0d7757705f93fa4b4e033089c42eaf5ad48cef5 --after-binary wal-e0d77577-red --after-sha256 7770fb790f2d32c4099979c37e0dafb590f3ea9d99329a83bcc4265ad486668c
```
