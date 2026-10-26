> RedDB compatibility distribution of upstream Tonic 0.14.6. The runtime
> uses current zstd bindings. See [UPSTREAM.md](UPSTREAM.md) for provenance,
> licensing and the Rust type-identity migration.

# tonic-prost

Prost codec implementation for [tonic] gRPC framework.

## Overview

This crate provides the `ProstCodec` for encoding and decoding protobuf messages using the [prost] library.

## Usage

This crate is typically used through the main `tonic` crate with the `prost` feature enabled (which is enabled by default).

```toml
[dependencies]
tonic = "0.14"
```

[tonic]: https://github.com/hyperium/tonic
[prost]: https://github.com/tokio-rs/prost