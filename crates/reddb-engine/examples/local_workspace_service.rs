//! Native test harness for the production desktop service entry point.

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if let Err(error) = reddb_engine::query::local_database::service::run(&arguments) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
