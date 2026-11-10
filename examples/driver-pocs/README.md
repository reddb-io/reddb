# Driver proof of concept journeys

This directory is the executable inventory for one useful journey through every
official language facade. Each PoC connects through the public API, creates a
table, inserts parameterized values, reads the row back, validates it, and
closes the connection.

The PoCs deliberately use the strongest mode that the facade actually ships
today. JavaScript, Python, Go, .NET, Java, Kotlin, PHP, Dart, C++, and Zig
exercise the in-process engine through their public facade. Rust currently uses
its live server journey in this inventory. A passing PoC
does **not** claim that the facade supports all four deployment modes; the
authoritative gaps remain in
[`deployment-mode-matrix.json`](../../docs/conformance/deployment-mode-matrix.json).

The implementation of each PoC lives with its SDK so it compiles against the
same source and test infrastructure as that SDK. [`manifest.json`](manifest.json)
maps the eleven official facades to those executable journeys and records the
standalone command for each one.

Validate the inventory without installing every language toolchain:

```sh
node scripts/verify-driver-pocs.mjs
```

For server-backed journeys, build `red` once and export its absolute path:

```sh
cargo build --locked --bin red
export RED_BIN="$PWD/target/debug/red"
export RED_SMOKE=1
```

Then run the command for the language in `manifest.json`. Embedded drivers
additionally need their native component built as described
by the corresponding driver README.
