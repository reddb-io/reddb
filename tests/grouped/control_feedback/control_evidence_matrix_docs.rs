const MATRIX: &str = include_str!("../../../docs/compliance/control-evidence-matrix.md");

#[test]
fn control_evidence_matrix_separates_current_foundations_from_required_capabilities() {
    let current = section_between(
        "## Current implemented and tested foundations",
        "## Required evidence surfaces",
    );
    let required = section_between("## Required capabilities still open", "## Non-goals");

    for implemented in [
        "`red.control_events`",
        "`red.registry`",
        "`red.registry_history`",
        "`red.managed_policies`",
        "`red.control_capabilities`",
        "`red.users`",
        "`red.api_keys`",
        "`red.query_audit`",
        "`export_evidence`",
    ] {
        assert!(
            current.contains(implemented),
            "implemented evidence surface should be a current foundation: {implemented}"
        );
    }

    for required_surface in [
        "`red.vault_metadata`",
        "`red.secret_events`",
        "`red.schema_events`",
        "`red.tenant_events`",
        "`red.policy_events`",
        "`red.backup_events`",
        "`red.restore_events`",
        "`red.replication_events`",
        "`red.config_events`",
        "`red.evidence_exports`",
    ] {
        assert!(
            required.contains(required_surface),
            "unimplemented evidence surface should remain a required capability: {required_surface}"
        );
        assert!(
            !current.contains(required_surface),
            "unimplemented evidence surface must not be listed as current: {required_surface}"
        );
    }
}

#[test]
fn control_evidence_matrix_documents_presets_scope_and_minimization() {
    for phrase in [
        "not automatic compliance certification",
        "`simple`",
        "`production`",
        "`regulated`",
        "simple preset does not enable regulated evidence overhead",
        "Raw query text is absent by default",
        "Secret plaintext is absent by default",
    ] {
        assert!(
            MATRIX.contains(phrase),
            "control evidence matrix should document: {phrase}"
        );
    }
}

fn section_between(start: &str, end: &str) -> &'static str {
    let start_index = MATRIX
        .find(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let after_start = start_index + start.len();
    let end_index = MATRIX[after_start..]
        .find(end)
        .map(|offset| after_start + offset)
        .unwrap_or_else(|| panic!("missing section end `{end}`"));
    &MATRIX[after_start..end_index]
}
