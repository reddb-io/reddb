# Native entity codec byte oracle

96 records: 8 deterministic entities for each store version 1 through 12.
Generated from the unchanged `UnifiedStore::write_entity_binary` and
`write_value_binary` at commit `2051ca6ba4e8a0c9235b3ca527b8a1f8c71634e4`,
before extracting the codec. Inputs: `../support/codec_corpus.rs`.

Each entry: little-endian u32 version, u32 corpus index, u32 payload length,
then the original entity payload. No header, padding or checksum.

SHA-256: `22c522ac8fe72e546191ce2d93eba7765d0e62c91c5a78a35042d77024f20695`.
