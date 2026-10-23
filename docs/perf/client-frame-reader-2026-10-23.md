# RedWire receive performance — 2026-10-23

The JS/TS client now retains incoming chunks until a complete frame is available,
then assembles only that frame. A contiguous frame needs no accumulator copy.
Fragmented input is copied once into a frame-sized buffer, rather than repeatedly
flattening an incomplete payload on every socket event. Consumed chunk references
are cleared immediately; reference-array compaction is amortized by waiting until
at least half the entries are consumed.

The final decoded payload remains owned, including for Node Buffer input, so
releasing the receive buffers does not invalidate a result. Empty views are ignored
instead of retaining their backing storage. A forced-GC child-process regression
keeps the reader and decoded payload alive while verifying that consumed frame and
accumulator buffers, plus empty views' backing buffers, can be collected.

EOF in a partial header/payload now rejects pending reads with CONNECTION_CLOSED.
Socket and framing errors reject all queued and future reads and release receive
buffers. Existing frame length limits, flag checks, error codes, wire bytes and
public signatures remain unchanged. This does not add backpressure or a limit on
total queued receive data when consumers are paused.

## Method

Baseline `8f1adc16f4c56188e5322f66521a8f620e5cb67e` is the reviewed v1.26.4 release
PR head; its reader source is identical to release commit
`880e37ff829dbc5c6efbdfbd0edd864f0af9ab02`. Measured production and harness code:
`71dd4b30d1d4a11ffb728c3c0bbae96a93d58fb1`. Subsequent changes add compression tests,
this report and optional-WASM summary formatting; they do not change the timed
reader or harness. The summary's existing WASM report output was verified identical.

One host: Intel Core i7-1065G7 / Linux x64, Node v26.12.1. Browser user agents and
all samples are in the [raw report](client-frame-reader-2026-10-23.json).

Each workload uses prebuilt bytes and a scripted anonymous handshake to obtain the
production reader. The timer covers socket-event delivery, frame accumulation,
payload decoding and Promise delivery. Input construction, handshake and complete
byte/metadata comparisons are outside timing. Each variant warms up, then 20 paired
samples alternate their execution order, targeting at least 20 ms per sample.
Ratios compare medians; intervals use the existing 2,000 paired-bootstrap resamples
and deterministic seed from `bench/summarize.mjs`.

These are actual Node/Chromium/Firefox/WebKit runtimes with synthetic socket events.
They do not measure TCP, browser WebSocket message delivery, database execution or
end-to-end query latency. Browser WebSocket fragmentation is not equivalent to TCP
fragmentation: network frames can be reassembled before a browser message event.
WebKit is Playwright's Linux port, not Safari/iOS. Small Node regression tests also
ran briefly during this exploratory capture; the intervals below retain all timing
variation. This is one run per runtime, not a population of devices or processes.

## Receive processing time

Coalesced rows measure delivery of all 128 frames; other rows measure one frame.
All bytes include the 16-byte frame header.

| Runtime | Input | Original µs | JS µs | JS speedup [95% CI] |
| --- | --- | ---: | ---: | ---: |
| node | contiguous-1KiB | 3.82 | 2.14 | 1.78× [1.70, 1.87] |
| node | contiguous-1MiB | 320.29 | 131.72 | 2.43× [2.20, 2.58] |
| node | fragmented-1MiB-64KiB | 1452.62 | 310.14 | 4.68× [4.34, 5.21] |
| node | fragmented-1MiB-1KiB | 86584.00 | 367.09 | 235.87× [186.71, 252.06] |
| node | coalesced-128x1KiB | 257.70 | 235.34 | 1.09× [0.95, 1.18] |
| node | bytewise-64B | 48.39 | 7.88 | 6.14× [5.33, 6.97] |
| chromium | contiguous-1KiB | 2.57 | 1.97 | 1.30× [1.25, 1.40] |
| chromium | contiguous-1MiB | 909.09 | 436.96 | 2.08× [1.95, 2.22] |
| chromium | fragmented-1MiB-64KiB | 3450.00 | 944.37 | 3.65× [3.47, 3.88] |
| chromium | fragmented-1MiB-1KiB | 209550.00 | 1020.00 | 205.44× [195.49, 210.98] |
| chromium | coalesced-128x1KiB | 201.15 | 138.42 | 1.45× [1.31, 1.65] |
| chromium | bytewise-64B | 32.08 | 8.48 | 3.78× [3.72, 3.90] |
| firefox | contiguous-1KiB | 21.53 | 20.18 | 1.07× [1.01, 1.10] |
| firefox | contiguous-1MiB | 1173.20 | 645.83 | 1.82× [0.77, 2.11] |
| firefox | fragmented-1MiB-64KiB | 4400.00 | 976.19 | 4.51× [4.19, 4.84] |
| firefox | fragmented-1MiB-1KiB | 219500.00 | 1250.00 | 175.60× [144.88, 198.90] |
| firefox | coalesced-128x1KiB | 1047.62 | 956.19 | 1.10× [1.00, 1.15] |
| firefox | bytewise-64B | 461.32 | 180.26 | 2.56× [1.94, 2.82] |
| webkit | contiguous-1KiB | 1.92 | 1.33 | 1.44× [1.37, 1.54] |
| webkit | contiguous-1MiB | 1171.57 | 213.15 | 5.50× [5.18, 7.16] |
| webkit | fragmented-1MiB-64KiB | 6583.33 | 1708.33 | 3.85× [3.07, 4.41] |
| webkit | fragmented-1MiB-1KiB | 119500.00 | 1052.63 | 113.53× [109.25, 147.05] |
| webkit | coalesced-128x1KiB | 129.87 | 113.32 | 1.15× [1.08, 1.23] |
| webkit | bytewise-64B | 27.10 | 11.84 | 2.29× [1.75, 2.78] |

## Accumulator copy traffic

All four runtimes produced the same separately instrumented copy counts:

| Input | Original copied bytes | New copied bytes |
| --- | ---: | ---: |
| Contiguous 1 KiB | 1,024 | 0 |
| Contiguous 1 MiB | 1,048,576 | 0 |
| 1 MiB in 64 KiB chunks | 8,912,896 | 1,048,576 |
| 1 MiB in 1 KiB chunks | 537,395,200 | 1,048,576 |
| 128 coalesced 1 KiB frames | 131,072 | 0 |
| 64 bytes in single-byte chunks | 2,080 | 64 |

Counts sum accumulator Uint8Array.set writes, excluding the decoder's final payload
slice. They are cumulative copy traffic, not live memory, RSS or peak heap. The
1 KiB-fragmented 1 MiB case falls from 512.5 MiB of accumulator copies to 1 MiB;
that extreme fragmentation should not be treated as a typical application gain.
No total browser/process memory reduction was measured.

## Decision and limits

Ship the chunk queue and consumed-buffer release without adding WASM or new runtime
dependencies. Fragmented-frame improvements are established in all tested runtimes.
Node's coalesced case and Firefox's contiguous 1 MiB case remain inconclusive: their
intervals include regressions. Other modest gains also need representative workloads
and more processes/devices before broad claims. The complete cells stay visible.

Tests cover every two-piece header/payload split, one-byte chunks, nonzero view
offsets, coalesced Buffer ownership and a partial trailing frame, paused consumers,
invalid lengths/flags, socket errors, partial EOF, missing zstd and native-zstd
fragmentation where the runtime provides it. GC tests verify collection separately
from the timer. HTTP/gRPC transports, query execution, Worker transfer/startup,
mobile devices and total process memory are outside this measurement.

Reproduce with the frame commands in
[the benchmark README](../../drivers/js-client/bench/README.md#frame-accumulation).
