# Client encoding benchmark

Compare the complete normalization + QueryWithParams + uncompressed RedWire
frame pipeline. The measured WASM function includes wasm-bindgen's input and
output copies and calls the canonical `reddb-wire` codecs. It supports the
single Float32Array/Uint8Array cases used here and is measurement-only; it is
not installed as the SDK backend.

Run from the repository root with Rust's pinned toolchain, the
`wasm32-unknown-unknown` target and wasm-bindgen CLI 0.2.120 available:

```sh
pnpm install --frozen-lockfile --ignore-scripts --filter @reddb-io/browser
pnpm --filter @reddb-io/browser exec playwright install --with-deps chromium firefox webkit
node drivers/js-client/bench/build-wasm.mjs
git worktree add --detach ../client-codec-baseline <baseline-commit>
REDDB_BENCH_BASELINE=../client-codec-baseline REDDB_BENCH_OUTPUT=client-codecs.json \
  node drivers/js-client/bench/run.mjs
node drivers/js-client/bench/summarize.mjs client-codecs.json
```

Each workload warms all implementations, then rotates their order across 20
paired samples of at least 20 ms. Every implementation must emit the original
client's exact bytes before measurement starts. Smaller CI sample counts only
check execution/parity; timings never gate shared runners. The summary uses a
deterministic paired bootstrap with 2,000 resamples of median speedup.

The report records raw samples, revisions, host/runtime versions, one observed
cold initialization, raw/gzip module size and final WASM linear memory (which
does not shrink after growing). Timing excludes network I/O and database query
execution. These are client CPU measurements on one host; WebKit here is the
Playwright Linux port, not a Safari/iOS qualification. No Worker is used by
this synchronous client-codec path.

Data-plane sketch: a 1,536-dimensional vector has 6 KiB of f32 payload. Retaining
its typed view avoids a 12 KiB JS number array and a reconstructed 6 KiB f32
array per request. Rust fills a pre-sized byte region instead of extending the
output Vec once per component. Payload tags, endianness and frame bytes remain
unchanged.

## Frame accumulation

Compare the production receive reader against a separate baseline checkout:

```sh
REDDB_BENCH_BASELINE=../client-frame-baseline REDDB_BENCH_OUTPUT=client-frames.json \
  node drivers/js-client/bench/run-frames.mjs
```

This runner needs the Playwright dependencies above, but does not build or load
WASM. It exercises contiguous, fragmented, coalesced and single-byte input in
Node, Chromium, Firefox and WebKit. Twenty paired samples alternate implementation
order, with at least 20 ms per sample. Input creation, anonymous handshake and full
byte comparisons are outside the timer; accumulation, payload decoding and Promise
delivery are inside. It counts accumulator `Uint8Array.set` writes separately,
excluding the decoder's final payload slice. These counts are cumulative memory
traffic, not peak RAM. Socket events are scripted; this is not a network, WebSocket
delivery or end-to-end database benchmark. WebKit is Playwright's Linux port.
