//! Pinned SchemaVocabulary lookup snapshots (issue #120).
//!
//! Each test seeds a small synthetic catalog into a fresh
//! `SchemaVocabulary`, runs `lookup` for a representative token, and
//! pins the deterministic, sorted formatting of the resulting hits.
//!
//! Snapshots flow through the shared `secret_redactor` filter chain
//! so the audit lint at `tests/snapshot_redaction_lint.rs` stays
//! clean.
//!
//! Workflow: `cargo insta accept` on first run, `cargo insta review`
//! when intentionally changing the output.

mod support {
    pub mod parser_hardening;
}

use reddb_server::runtime::schema_vocabulary::{DdlEvent, SchemaVocabulary, VocabHit};
use support::parser_hardening::secret_redactor;

fn fmt_hits(token: &str, hits: &[VocabHit]) -> String {
    let mut sorted = hits.to_vec();
    sorted.sort_by(|a, b| {
        a.collection
            .cmp(&b.collection)
            .then_with(|| a.column.cmp(&b.column))
            .then_with(|| a.type_tag.cmp(&b.type_tag))
    });
    let mut out = String::new();
    out.push_str(&format!("token: {:?}\n", token));
    out.push_str(&format!("hit_count: {}\n", sorted.len()));
    for hit in sorted {
        out.push_str(&format!(
            "- collection: {} | column: {:?} | type_tag: {:?}\n",
            hit.collection, hit.column, hit.type_tag,
        ));
    }
    out
}

/// Build the representative fixture used across the pinned scenarios.
///
/// Three collections that exercise: collection-name hit, shared
/// column across collections, doc-shape type tag, index name,
/// policy name, and an accent-folded variant.
fn fixture() -> SchemaVocabulary {
    let mut vocab = SchemaVocabulary::new();
    vocab.on_ddl(DdlEvent::CreateCollection {
        collection: "users".to_string(),
        columns: vec![
            "id".to_string(),
            "email".to_string(),
            "country".to_string(),
        ],
        type_tags: Vec::new(),
        description: None,
    });
    vocab.on_ddl(DdlEvent::CreateCollection {
        collection: "passports".to_string(),
        columns: vec![
            "id".to_string(),
            "country".to_string(),
            "expires_at".to_string(),
        ],
        type_tags: vec!["diplomatic".to_string()],
        description: None,
    });
    vocab.on_ddl(DdlEvent::CreateCollection {
        collection: "events".to_string(),
        columns: vec!["id".to_string(), "type".to_string(), "payload".to_string()],
        type_tags: vec!["payment".to_string(), "refund".to_string()],
        description: None,
    });
    vocab.on_ddl(DdlEvent::CreateIndex {
        collection: "users".to_string(),
        index: "idx_users_email".to_string(),
        columns: vec!["email".to_string()],
    });
    vocab.on_ddl(DdlEvent::CreatePolicy {
        collection: "users".to_string(),
        policy: "tenant_isolation".to_string(),
    });
    vocab
}

#[test]
fn lookup_passport_resolves_collection() {
    let _guard = secret_redactor::install_redactions();
    let vocab = fixture();
    let hits = vocab.lookup("passport");
    // The collection is named `passports`; the singular form does
    // *not* match because the index keys on the literal token. The
    // pinned snapshot makes that intent explicit.
    insta::assert_snapshot!(
        "lookup_passport_resolves_collection",
        fmt_hits("passport", &hits)
    );
}

#[test]
fn lookup_passports_resolves_collection() {
    let _guard = secret_redactor::install_redactions();
    let vocab = fixture();
    let hits = vocab.lookup("passports");
    insta::assert_snapshot!(
        "lookup_passports_resolves_collection",
        fmt_hits("passports", &hits)
    );
}

#[test]
fn lookup_id_resolves_every_collection() {
    let _guard = secret_redactor::install_redactions();
    let vocab = fixture();
    let hits = vocab.lookup("id");
    insta::assert_snapshot!(
        "lookup_id_resolves_every_collection",
        fmt_hits("id", &hits)
    );
}

#[test]
fn lookup_email_resolves_collection_and_index() {
    let _guard = secret_redactor::install_redactions();
    let vocab = fixture();
    let hits = vocab.lookup("email");
    insta::assert_snapshot!(
        "lookup_email_resolves_collection_and_index",
        fmt_hits("email", &hits)
    );
}

#[test]
fn lookup_accent_folded_variant_matches_passports() {
    let _guard = secret_redactor::install_redactions();
    let vocab = fixture();
    // `pässpörts` is the accent-folded sibling of `passports`. The
    // snapshot pins the normaliser's behaviour on a real fixture.
    let hits = vocab.lookup("pässpörts");
    insta::assert_snapshot!(
        "lookup_accent_folded_variant_matches_passports",
        fmt_hits("pässpörts", &hits)
    );
}
