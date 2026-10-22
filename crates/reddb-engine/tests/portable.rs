#[path = "support/portable.rs"]
mod portable;

#[test]
fn portable_execution_conformance() {
    for (name, run) in portable::CASES {
        eprintln!("{name}");
        run();
    }
}
