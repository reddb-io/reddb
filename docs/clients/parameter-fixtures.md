# Parameter Fixture Conformance

`crates/reddb-wire/tests/fixtures/params/manifest.json` is the canonical
fixture manifest for RedWire `QueryWithParams` parameter encoding.

The manifest pins:

- one encoded value for every RedWire `Value` tag: null, bool, int, float,
  text, bytes, vector, json, timestamp, and uuid
- boundary values for i64, non-finite and subnormal f64, empty bytes, nested
  canonical JSON, timestamp max, uuid bytes, empty vector, and f32 vector bytes
- full `QueryWithParams` payload fixtures, including the SQL header and
  back-to-back encoded parameter values

When adding or changing a wire `Value` variant:

1. Add a new manifest entry with a stable `name`, `kind`, and lowercase
   `redwire_hex`.
2. Add at least one query payload fixture if the change affects the
   `QueryWithParams` frame layout.
3. Update each official driver conformance test to construct the native value
   for the new fixture name and assert byte-identical output.
4. Run the language test for every driver touched plus
   `cargo test -p reddb-io-wire --test params_fixtures`.

The CI `Driver Param Conformance` job currently runs the shared manifest
against the JS and Go RedWire codecs. Other driver tests should use the same
manifest rather than copying expected bytes inline.
