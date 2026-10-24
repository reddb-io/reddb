# Bun receive performance — 2026-10-24 UTC

The Bun driver queues owned receive chunks until a complete RedWire frame is
available. It discards the header and copies the payload directly into an owned
result buffer. This removes repeated whole-accumulator Buffer.concat calls and
immediately clears references to consumed chunks. Reference-array compaction waits
until at least half the entries are consumed, so reference movement is amortized.
The native TCP callback's existing Buffer.from(data) ownership copy is retained.

Terminal framing/socket errors, remote EOF and local close now release buffers and
reject pending/future requests. Closing is idempotent. Early connection failures
are preserved across socket setup, and failed handshakes close their socket.
Engine error frames reject their own request while the connection remains usable.
A locally oversized request is encoded before it enters the pending queue: its
rejection preserves other in-flight requests and subsequent connection use.

Query signatures, UTF-8 strings, JSON/binary bulk counts, capability negotiation,
wire encoding, existing flag handling and the 16 MiB frame limit are preserved.
This does not add a receive-backlog cap, new multiplexing or backpressure policy.

## Method

Baseline `31788e2bc10d33bea47166c3de71f350e539f43c` has Bun source identical to main
commit `8fff87aeea7c313773e67178b1c4c877c3e697b6` and the v1.26.4 implementation.
Measured code and harness: `a54ff5c976d79c1b6c8cd532de3e724e57f09386`. Later changes add
a zero-payload/bulk ownership regression and this report; they do not change the
timed production code or harness.

One host: Intel Core i7-1065G7 / Linux x64, Bun 1.4.4. The [raw report](bun-receive-2026-10-24.json)
contains every sample, copy count and exact runtime/revision metadata.

Each variant uses the actual production query() method on a scripted socket, with
legacy capability state and prebuilt response bytes. Timings include SQL/request
encoding, pending-queue work, receive parsing, owned payload creation, UTF-8 string
conversion and Promise delivery. There is no socket I/O or database execution.
Input construction and exact ASCII-response comparisons are outside the timer.
Native TCP tests independently exercise the real Bun socket callbacks, including
the unchanged transport ownership copy; this copy is outside the timed fixture.

After warmup, 20 paired samples alternate implementation order, with at least
20 ms per sample. Ratios compare medians; intervals use 2,000 deterministic paired
bootstrap resamples from the existing JS benchmark summarizer. The earlier capture
was repeated after moving local frame-size validation ahead of pending-queue insertion;
the final report measures that corrected production version.

## Scripted query processing time

Coalesced rows measure all 128 results; other rows measure one result. Sizes include
the 16-byte frame header. These are component times, not network or database-query
latency/throughput claims.

| Runtime | Input | Original µs | JS µs | JS speedup [95% CI] |
| --- | --- | ---: | ---: | ---: |
| bun | contiguous-1KiB | 1.98 | 1.50 | 1.31× [1.21, 1.40] |
| bun | contiguous-1MiB | 412.21 | 298.65 | 1.38× [1.29, 1.44] |
| bun | fragmented-1MiB-64KiB | 1712.40 | 346.38 | 4.94× [4.65, 5.21] |
| bun | fragmented-1MiB-1KiB | 67007.13 | 395.24 | 169.53× [149.46, 190.51] |
| bun | coalesced-128x1KiB | 179.74 | 154.80 | 1.16× [1.13, 1.27] |
| bun | bytewise-64B | 5.83 | 4.02 | 1.45× [1.40, 1.50] |

## Receive copy traffic

A separate probe instruments top-level Buffer.concat, Buffer.from(view) and
Buffer.copy calls during receive delivery. Nested built-in calls are suppressed
to avoid double counting. It includes the result-ownership copy and any split
header scratch copy; it excludes outbound encoding and the native TCP callback
ownership copy. These are cumulative copied bytes, not peak heap, RSS or live data.

| Input | Baseline copied bytes | New copied bytes |
| --- | ---: | ---: |
| Contiguous 1 KiB | 2,032 | 1,008 |
| Contiguous 1 MiB | 2,097,136 | 1,048,560 |
| 1 MiB in 64 KiB chunks | 9,961,456 | 1,048,560 |
| 1 MiB in 1 KiB chunks | 538,443,760 | 1,048,560 |
| 128 coalesced 1 KiB frames | 260,096 | 129,024 |
| 64 bytes in single-byte chunks | 2,128 | 53 |

The extreme 1 KiB-fragmented 1 MiB case drops from approximately 513.5 MiB of
receive copies to one payload of approximately 1 MiB. Of that baseline, 512.5 MiB
was accumulator concatenation alone. This is not a typical application speedup
or a measurement of resident memory. A separate Bun.gc child-process regression
keeps the connection and result string alive while verifying collection of
consumed receive/accumulator storage and empty views' backing buffers.

## Package contents

A files whitelist ships only the runtime entry, README and changelog (package.json
is included automatically). New and existing development tests/benchmarks stay in
the repository. A real npm pack tarball was unpacked, and its standalone index.ts
completed a scripted query under Bun without fixtures or sibling packages.

| Local candidate package | Baseline | New |
| --- | ---: | ---: |
| Tarball bytes | 11384 | 9429 |
| Unpacked bytes | 32827 | 26292 |
| Files | 8 | 4 |

Both pack probes use the current 1.26.4 manifest version. These are local candidate
artifacts, not a newly published npm version; version/changelog updates can change
final package sizes.

## Validation and limits

Sixteen Bun tests cover every two-piece UTF-8/header split, bytewise data,
coalesced FIFO responses, payload ownership, empty results, JSON/binary bulk counts,
length bounds, per-query engine errors, local size rejection, close/error/write
failures and GC. Actual native Bun TCP peers exercise a large result, truncated
response, orderly close, malformed framing and close before authentication.
The complete package test command also passes shared embedded-SDK parameters,
legacy capability fallback and bound/unbound queries against the published RedDB
1.26.4 binary. The wire crate passes cargo check; the required-check/CI-shim
contract tests pass. CI runs Bun tests on PRs alongside the existing server build
and checks benchmark execution/response parity without timing gates.

All six paired intervals exceed 1 in this capture. This is one final run on one
host/runtime version, not a regression guarantee across devices or Bun versions.
OS packet coalescing means actual native TCP delivery can differ from scripted
fragment sizes. TLS, real network/engine latency, total process memory and other
language drivers are not measured here. The extra small-frame queue/dispatch work
and package footprint should be remeasured with representative applications.

Reproduce using the commands in [the Bun README](../../drivers/bun/README.md).
