//! Compatibility facade for the vector B-tree persisted page format.

pub use reddb_file::vector_btree_page_format::{
    decode_leaf_cell, encode_leaf_cell_v1, encode_leaf_cell_v2, LeafCell, LeafCellFlags,
    PageFormatError, PageHeader, PageType, FORMAT_VERSION, FORMAT_VERSION_V1, FORMAT_VERSION_V2,
    PAGE_HEADER_SIZE,
};
