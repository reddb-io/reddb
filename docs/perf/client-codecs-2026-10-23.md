# JS/TS client encoding performance — 2026-10-23

The production change preserves typed vectors/byte views through RedWire query
normalization, avoiding JS array and base64 round trips. HTTP/gRPC retain their
JSON-compatible representation. The transport releases input-view references
after encoding and before network suspension; a separate pending-request GC
regression verifies that a 1 MiB caller backing buffer can then be collected.
This retention check is outside the timed encoding pipeline below.
The canonical Rust vector encoder fills a
pre-sized output region with identical little-endian bytes, including signed
zero, infinities and NaN payloads.

The WASM bridge is a measurement-only example that calls `reddb-wire`; the
published JS client still uses its JavaScript codec. It handles the single
Float32Array/Uint8Array parameter cases below, not the full client contract.

## Method

Baseline `2176fc0545aabac0a02222c5b6b75c0103604e9b`; measured code `ca94b3d26bc7c4fa0833a5255b9b81c4fda5bfb4`. Host:
Intel(R) Core(TM) i7-1065G7 CPU @ 1.30GHz / linux x64.
Node v26.12.1; browser user agents are in the
[raw report](client-codecs-2026-10-23.json).

Each case warms all three paths, rotates their order over 20 paired samples
of at least 20 ms and checks byte-for-byte equality with the original JS
request. Measurements include parameter normalization, the complete request
frame, JS/WASM copies and allocations. They exclude network and server work.
Reported speedups are ratios of medians; 95% intervals use 2,000 deterministic
paired bootstrap resamples. This is one host and one browser process per engine.
WebKit means Playwright's Linux port, not real Safari/iOS qualification.

## Warm encoding results

| Runtime | Input | Original µs | JS µs | WASM µs | JS speedup [95% CI] | WASM speedup [95% CI] |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| node | vector-8 | 6.55 | 5.83 | 1.70 | 1.12× [1.10, 1.20] | 3.85× [3.73, 4.19] |
| node | vector-1536 | 54.02 | 6.80 | 2.74 | 7.94× [7.00, 8.56] | 19.68× [18.33, 22.21] |
| node | vector-65536 | 1917.83 | 228.39 | 85.91 | 8.40× [7.68, 8.97] | 22.32× [20.76, 23.95] |
| node | bytes-1024 | 11.23 | 11.02 | 2.63 | 1.02× [0.46, 2.12] | 4.27× [1.80, 6.75] |
| node | bytes-262144 | 378.28 | 192.22 | 112.21 | 1.97× [1.33, 3.17] | 3.37× [2.16, 5.73] |
| chromium | vector-8 | 5.03 | 4.06 | 1.14 | 1.24× [1.03, 1.48] | 4.39× [3.68, 4.83] |
| chromium | vector-1536 | 62.81 | 15.51 | 4.65 | 4.05× [3.65, 4.33] | 13.52× [12.81, 14.99] |
| chromium | vector-65536 | 2303.13 | 492.71 | 180.36 | 4.67× [4.13, 5.81] | 12.77× [11.72, 15.04] |
| chromium | bytes-1024 | 23.89 | 4.84 | 1.59 | 4.94× [4.46, 5.87] | 15.02× [13.30, 17.80] |
| chromium | bytes-262144 | 4696.88 | 318.75 | 137.19 | 14.74× [13.74, 15.71] | 34.24× [32.24, 37.07] |
| firefox | vector-8 | 4.14 | 3.94 | 2.69 | 1.05× [0.98, 1.20] | 1.54× [1.34, 1.74] |
| firefox | vector-1536 | 37.88 | 15.43 | 11.21 | 2.45× [2.17, 2.92] | 3.38× [2.80, 3.95] |
| firefox | vector-65536 | 1500.00 | 437.50 | 343.75 | 3.43× [3.27, 4.03] | 4.36× [4.17, 4.57] |
| firefox | bytes-1024 | 32.05 | 3.68 | 3.24 | 8.72× [6.89, 9.70] | 9.89× [8.87, 11.70] |
| firefox | bytes-262144 | 9125.00 | 335.94 | 111.51 | 27.16× [25.04, 29.40] | 81.83× [62.57, 87.60] |
| webkit | vector-8 | 2.94 | 2.68 | 1.02 | 1.10× [0.92, 1.39] | 2.88× [2.46, 3.34] |
| webkit | vector-1536 | 40.32 | 7.33 | 2.30 | 5.50× [5.25, 5.87] | 17.53× [16.55, 18.51] |
| webkit | vector-65536 | 1500.00 | 187.50 | 73.53 | 8.00× [7.32, 8.40] | 20.40× [19.55, 21.60] |
| webkit | bytes-1024 | 32.05 | 2.82 | 1.27 | 11.38× [10.95, 12.08] | 25.24× [23.75, 27.31] |
| webkit | bytes-262144 | 7375.00 | 116.48 | 50.00 | 63.32× [56.19, 66.00] | 147.50× [138.00, 151.88] |

## Initialization and memory

The prototype is 515,973 bytes raw / 138,495 bytes gzip.
Its final linear memory was 2 MiB in each runtime. That is WASM linear memory,
not a measurement of total JS heap/process memory. One observed initialization
per fresh runtime was:

| Runtime | Initialization ms |
| --- | ---: |
| node | 2.43 |
| chromium | 15.60 |
| firefox | 42.00 |
| webkit | 22.00 |

Browser startup includes module loading from localhost, compilation and
instantiation; Node uses already-read WASM bytes. These single observations
are not a startup latency distribution or an Internet-download measurement.
No Worker is introduced by this synchronous codec path.

## Decision and limits

Ship typed-view normalization automatically for RedWire. Large-vector encoding
improves in all measured runtimes. Tiny inputs show smaller or inconclusive
gains: Node's 1 KiB byte case has a wide interval including regressions, and the
smallest WebKit/Firefox vector cases do not establish a meaningful improvement.
Those cells remain visible above.

Keep the WASM bridge experimental. Its warm path is promising, but initialization
and module transfer must be amortized, and full parameter/result/error/stream
parity is still required. For example, the observed Firefox initialization would
take roughly 10,000 encodings of the 1,536-dimensional vector to amortize against
the optimized JS path, excluding network transfer. The 65,536-dimensional Firefox
case also has a much smaller WASM advantage than the other runtimes.

These are encoding CPU gains, not database-query latency or throughput claims.
The benchmark and reproduction instructions are in
[drivers/js-client/bench](../../drivers/js-client/bench/README.md). The data-plane
sketch there accounts for the avoided typed-vector allocations. Actual transport
latency, mobile devices, Bun/Deno, total memory and sustained application workloads
remain unmeasured.
