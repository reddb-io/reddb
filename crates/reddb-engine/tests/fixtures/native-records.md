# Native record materialization oracle

112 results: eight native materialization modes over 14 deterministic entities.
Generated from `runtime/record_search.rs` at commit
`67d70a34053e99dbf7e1b53a1b238789f446cf9a`, before extraction.
Inputs: `../support/record_corpus.rs` and `../support/codec_corpus.rs`.
The oracle includes full, lean, projected and universal materialization, each
by value and by reference. Existing differences between these paths are retained.
Fields use the canonical `Value::to_json` representation; object order is ignored.

SHA-256: `c0df58240d9dc41361fcf4ab996c28f66da84a08537f96997d97f1758bf36312`.
