const ADR: &str = include_str!("../.red/adr/0058-cluster-bootstrap-authority.md");
const FIRST_BOOT: &str = include_str!("../docs/deployment/first-boot.md");

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected documentation to contain: {needle}"
    );
}

#[test]
fn cluster_bootstrap_authority_model_is_recorded() {
    for required in [
        "reserved global system range owner",
        "global auth, vault, config, and policy state",
        "durable bootstrap-complete marker",
        "current lease/term owner",
        "compare-and-set",
        "Non-owner members must not bootstrap independently",
        "wait, forward or redirect",
        "roll partial state forward idempotently",
        "`--no-auth` / `--dev`",
        "RedDB Cloud keeps policy-first bootstrap",
    ] {
        assert_contains(ADR, required);
    }
}

#[test]
fn first_boot_page_points_cluster_bootstrap_at_reserved_range_owner() {
    for required in [
        "Cluster auth/vault/config/policy first boot uses the reserved global system range owner",
        "The reserved range stores global auth, vault, config, policy state, and `system.bootstrap.completed`",
        "Non-owner members must not run presets, create initial admins, or initialize vault material",
        "Anonymous `--no-auth` / `--dev` cluster-shaped boot remains an explicit development carveout",
    ] {
        assert_contains(FIRST_BOOT, required);
    }
}
