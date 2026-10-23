# JS/TS client encoding performance — 2026-10-23

The production change preserves typed vectors/byte views through RedWire query
normalization, avoiding JS array and base64 round trips. HTTP/gRPC retain their
JSON-compatible representation. The canonical Rust vector encoder fills a
pre-sized output region with identical little-endian bytes, including signed
zero, infinities and NaN payloads.

The WASM bridge is a measurement-only example that calls `reddb-wire`; the
published JS client still uses its JavaScript codec. It handles the single
Float32Array/Uint8Array parameter cases below, not the full client contract.

## Method

Baseline `2176fc0545aabac0a02222c5b6b75c0103604e9b`; measured code `5244875d4c56b9dd071d34e82977d3a528f658f6`. Host:
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
| node | vector-8 | 6.22 | 5.52 | 1.52 | 1.13× [1.04, 1.25] | 4.08× [3.81, 4.46] |
| node | vector-1536 | 54.31 | 7.03 | 2.70 | 7.72× [7.10, 8.29] | 20.09× [18.32, 21.84] |
| node | vector-65536 | 1925.18 | 230.39 | 86.00 | 8.36× [7.82, 8.81] | 22.39× [20.80, 23.95] |
| node | bytes-1024 | 8.25 | 8.64 | 1.94 | 0.96× [0.39, 2.05] | 4.27× [1.41, 8.24] |
| node | bytes-262144 | 201.23 | 112.21 | 61.92 | 1.79× [1.47, 2.09] | 3.25× [2.75, 3.75] |
| chromium | vector-8 | 4.77 | 3.70 | 1.10 | 1.29× [1.19, 1.50] | 4.33× [3.91, 4.83] |
| chromium | vector-1536 | 61.05 | 14.40 | 4.38 | 4.24× [3.68, 4.50] | 13.94× [12.96, 15.91] |
| chromium | vector-65536 | 2043.75 | 433.33 | 156.64 | 4.72× [4.46, 4.90] | 13.05× [12.08, 13.79] |
| chromium | bytes-1024 | 27.55 | 4.59 | 1.59 | 6.01× [5.08, 7.63] | 17.37× [15.19, 19.67] |
| chromium | bytes-262144 | 4543.75 | 313.28 | 128.75 | 14.50× [13.88, 15.32] | 35.29× [33.23, 36.37] |
| firefox | vector-8 | 4.06 | 3.87 | 2.86 | 1.05× [0.98, 1.17] | 1.42× [1.38, 1.55] |
| firefox | vector-1536 | 37.88 | 14.81 | 11.47 | 2.56× [2.15, 3.25] | 3.30× [2.68, 3.82] |
| firefox | vector-65536 | 1562.50 | 382.81 | 359.38 | 4.08× [3.41, 4.76] | 4.35× [4.26, 4.55] |
| firefox | bytes-1024 | 31.25 | 3.59 | 3.32 | 8.70× [6.88, 9.09] | 9.42× [8.68, 11.47] |
| firefox | bytes-262144 | 9125.00 | 261.46 | 125.00 | 34.90× [27.16, 42.57] | 73.00× [65.58, 84.83] |
| webkit | vector-8 | 2.72 | 2.77 | 0.84 | 0.98× [0.89, 1.05] | 3.23× [2.98, 3.43] |
| webkit | vector-1536 | 41.67 | 7.51 | 2.39 | 5.55× [5.26, 5.83] | 17.40× [16.84, 18.00] |
| webkit | vector-65536 | 1812.50 | 250.00 | 100.96 | 7.25× [6.44, 7.91] | 17.95× [14.50, 19.66] |
| webkit | bytes-1024 | 48.08 | 4.59 | 1.73 | 10.48× [8.85, 11.69] | 27.71× [25.31, 28.99] |
| webkit | bytes-262144 | 7718.75 | 138.89 | 65.79 | 55.58× [53.07, 58.14] | 117.33× [111.60, 125.00] |

## Initialization and memory

The prototype is 515,973 bytes raw / 138,495 bytes gzip.
Its final linear memory was 2 MiB in each runtime. That is WASM linear memory,
not a measurement of total JS heap/process memory. One observed initialization
per fresh runtime was:

| Runtime | Initialization ms |
| --- | ---: |
| node | 2.34 |
| chromium | 17.30 |
| firefox | 41.00 |
| webkit | 19.00 |

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
take roughly 12,000 encodings of the 1,536-dimensional vector to amortize against
the optimized JS path, excluding network transfer. The 65,536-dimensional Firefox
case also has a much smaller WASM advantage than the other runtimes.

These are encoding CPU gains, not database-query latency or throughput claims.
The benchmark and reproduction instructions are in
[drivers/js-client/bench](../../drivers/js-client/bench/README.md). The data-plane
sketch there accounts for the avoided typed-vector allocations. Actual transport
latency, mobile devices, Bun/Deno, total memory and sustained application workloads
remain unmeasured.
