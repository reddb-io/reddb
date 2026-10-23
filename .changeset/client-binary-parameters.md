---
"@reddb-io/client": patch
"@reddb-io/cli": patch
---

Preserve typed vectors and byte buffers through RedWire query normalization to avoid redundant array and base64 conversions. Fill canonical Rust vector payloads in one pre-sized region. Keep JSON transports and wire bytes unchanged, and compare the complete encoding path with canonical Rust/WASM codecs.
