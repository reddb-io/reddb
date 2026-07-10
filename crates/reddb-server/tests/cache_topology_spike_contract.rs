#[test]
fn cache_topology_spike_artifacts_cover_issue_1970_contract() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("server crate lives under crates/reddb-server");

    let cargo_toml = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("server Cargo.toml must be readable");
    assert!(
        cargo_toml.contains("name = \"cache_topology_spike_bench\""),
        "Criterion bench target must be registered"
    );

    let bench = std::fs::read_to_string(
        manifest_dir
            .join("benches")
            .join("cache_topology_spike_bench.rs"),
    )
    .expect("cache topology Criterion bench must exist");
    for workload in [
        "point-read-hot-l1",
        "point-read-zipfian-l2",
        "cold-scan",
        "mixed-write-heavy",
    ] {
        assert!(
            bench.contains(workload),
            "bench must cover workload {workload}"
        );
    }
    for candidate in [
        "baseline-shipped",
        "unified-slot-arena",
        "promote-on-second-hit",
    ] {
        assert!(
            bench.contains(candidate),
            "bench must cover candidate {candidate}"
        );
    }
    assert!(
        bench.contains("allocations_per_op"),
        "bench must report hit-path allocations/op"
    );

    let report = std::fs::read_to_string(
        repo_root
            .join("docs")
            .join("perf")
            .join("cache-topology-spike-2026-07-10.md"),
    )
    .expect("cache topology report must exist");
    assert!(report.contains("Measured commit:"));
    assert!(report.contains("Recommendation:"));
    assert!(report.contains("allocations/op"));
}
