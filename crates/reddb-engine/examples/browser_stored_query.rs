//! Native fixture generator for public SDK and installed TS/Vite consumers.
#[path = "../tests/support/local_read_fixture.rs"]
mod fixture;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use reddb_file::{OpenMode, StdVfs, Vfs};
    let path = std::env::args()
        .nth(1)
        .ok_or("expected native artifact path")?;
    fixture::create(StdVfs.open(std::path::Path::new(&path), OpenMode::create_keep())?)
}
