`native-metadata.bin` is the output of the original native `serialize_metadata`
writer in `crates/reddb-server/src/storage/unified/store/impl_pages.rs` at
`2f4411027`, before extraction. The inputs are `support/metadata_corpus.rs`.
The original writer functions were compiled unchanged except for importing the
relocated metadata types, then run once to freeze these bytes. Tests must compare
against this artifact; do not regenerate it from the new shared writer.

The corpus covers all twelve value tags, all five reference targets, signed and
unsigned boundaries, negative zero, UTF-8, nested arrays/objects, byte arrays,
timestamps and coordinates. Object and top-level keys are sorted by the writer.

`native-store-v12.rdst` was saved by the native `UnifiedStore::save_to_file` at
`bd335db1b`. It contains an empty collection, eight `codec_corpus::entities()`
in `codec_<id>` collections, the metadata above on every entity, their existing
references to entity 77, an additional RowToNode reference from 1024 to 1026,
and the auxiliary bytes `opaque catalog`. The native writer assigned resident
sequence numbers. Native and WASM recovery verify this same frozen snapshot;
the browser suite persists its exact bytes through OPFS and a full offline
browser restart. This is a test fixture, not a runtime database file.
