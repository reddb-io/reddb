//! Compatibility facade for the vector/B-tree persisted value codec.

pub use reddb_file::vector_value_codec::{
    decode, encode, would_encode_to, ValueCodecError, ValueFlag,
};
