//! Embedded integration policy around the canonical RedDB engine.
//!
//! Board adapters implement [`VfsFile`]. This crate selects the bounded paged
//! database; it does not fork SQL, the pager, B-tree, record codec or `.rdb`.

pub use reddb_engine as engine;
pub use reddb_engine::query::{
    local_database::PagedLocalDatabase as Database,
    local_read::{LocalReadError, StreamQueryResult},
};
pub use reddb_file as file;
pub use reddb_file::VfsFile;
pub use reddb_types as types;

pub mod fixed_file;
#[cfg(feature = "host-flash-model")]
pub mod flash_emulator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardProfile {
    pub name: &'static str,
    pub sram_bytes: usize,
    pub flash_bytes: usize,
    /// Usable database bytes after firmware and filesystem overhead.
    pub database_bytes: usize,
    pub rust_heap_bytes: usize,
    pub stack_bytes: usize,
    pub read_buffer_bytes: usize,
    pub cache_pages: usize,
}

pub const NANO_ESP32: BoardProfile = BoardProfile {
    name: "Arduino Nano ESP32",
    sram_bytes: 512 * 1024,
    flash_bytes: 16 * 1024 * 1024,
    database_bytes: 12_378_112,
    rust_heap_bytes: 64 * 1024,
    stack_bytes: 256 * 1024,
    read_buffer_bytes: engine::storage::page::PAGE_SIZE,
    cache_pages: 1,
};

pub const PICO_2: BoardProfile = BoardProfile {
    name: "Raspberry Pi Pico 2",
    sram_bytes: 520 * 1024,
    flash_bytes: 4 * 1024 * 1024,
    database_bytes: 2_621_440,
    rust_heap_bytes: 64 * 1024,
    stack_bytes: 320 * 1024,
    read_buffer_bytes: engine::storage::page::PAGE_SIZE,
    cache_pages: 1,
};
