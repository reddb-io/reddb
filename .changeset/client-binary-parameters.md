---
"@reddb-io/client": patch
---

Preserve typed vectors and byte buffers through RedWire query normalization to avoid redundant array and base64 conversions. Keep JSON transports unchanged and compare the complete encoding path with canonical Rust/WASM codecs.
