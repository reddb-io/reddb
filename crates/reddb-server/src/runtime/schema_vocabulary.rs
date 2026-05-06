//! Reverse-index from tokens to schema entities (issue #120).
//!
//! `SchemaVocabulary` holds a `token → Vec<VocabHit>` map. Lookups are
//! O(1) average per token (HashMap probe + slice return). The index is
//! kept current incrementally via `on_ddl(event)` calls from the DDL
//! execution paths in [`super::impl_ddl`], [`super::impl_timeseries`],
//! [`super::impl_queue`], [`super::impl_tree`], and the policy /
//! migration dispatch in [`super::impl_core`].
//!
//! The AskPipeline (slice C, issue #121) uses this index in its Stage 2
//! candidate-narrowing pass *before* spending embedding compute, so the
//! API exposes only the read path that the pipeline needs:
//! `lookup(token) -> &[VocabHit]`. Catalog ownership (mutating writes)
//! stays internal to the runtime.
//!
//! ## Token sources
//!
//! - Collection names
//! - Column names (declared in `CollectionContract`)
//! - Doc-shape `type` / `kind` discriminator values, when surfaced via
//!   the contract's enum metadata
//! - Index names
//! - Policy names
//! - Operator-supplied descriptions when present in the catalog
//!
//! ## Token normalisation
//!
//! 1. Unicode NFKD decomposition.
//! 2. Strip combining marks (accent-fold).
//! 3. Lowercase.
//! 4. Strip surrounding non-alphanumerics.
//!
//! `passport`, `PASSPORT`, and `pässpört` all normalise to `passport`.
//! Pinned by `tests::normalisation_*`.

use std::collections::{HashMap, HashSet};

/// Stable identifier alias used by issue #120's public API. Matches the
/// `String`-keyed collections used elsewhere in the runtime.
pub type CollectionId = String;
/// Column name alias.
pub type ColumnName = String;

/// One hit returned by a vocabulary lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VocabHit {
    pub collection: CollectionId,
    /// `None` when the token matched the collection name itself rather
    /// than one of its columns.
    pub column: Option<ColumnName>,
    /// Doc-shape `type` / `kind` discriminator value, when applicable.
    pub type_tag: Option<String>,
}

/// Catalog-shaped DDL events the index reacts to.
///
/// One enum per issue #120 acceptance row. The variants stay flat so
/// the dispatch sites in `impl_ddl` etc. don't need to share AST types.
#[derive(Debug, Clone)]
pub enum DdlEvent {
    /// CREATE TABLE / CREATE collection of any kind. `columns` is the
    /// list of declared column names (may be empty for dynamic
    /// collections); `type_tags` carries any catalog-known discriminator
    /// values (enum variants on a `type` / `kind` column).
    CreateCollection {
        collection: CollectionId,
        columns: Vec<ColumnName>,
        type_tags: Vec<String>,
        description: Option<String>,
    },
    /// ALTER TABLE — replaces the column / discriminator set for the
    /// collection. Implemented as a drop+recreate of the collection's
    /// entries to guarantee invalidation completeness.
    AlterCollection {
        collection: CollectionId,
        columns: Vec<ColumnName>,
        type_tags: Vec<String>,
        description: Option<String>,
    },
    /// DROP TABLE / DROP collection. Removes every entry whose
    /// `collection` field equals the dropped collection's name,
    /// including index / policy entries scoped to it.
    DropCollection { collection: CollectionId },
    /// CREATE INDEX. The token feed is the index name; `columns`
    /// captures the indexed columns so a token match can disambiguate
    /// between the index hit and the column hits.
    CreateIndex {
        collection: CollectionId,
        index: String,
        columns: Vec<ColumnName>,
    },
    /// DROP INDEX. Removes the index-name token entry.
    DropIndex {
        collection: CollectionId,
        index: String,
    },
    /// CREATE POLICY. Policy names are token sources for the AskPipeline
    /// when an operator asks "show me the rls policy that..." —
    /// matching `PolicyName` should resolve back to the table.
    CreatePolicy {
        collection: CollectionId,
        policy: String,
    },
    /// DROP POLICY.
    DropPolicy {
        collection: CollectionId,
        policy: String,
    },
}

/// Reverse index `token -> Vec<VocabHit>`.
///
/// `lookup` is O(token count): one HashMap probe to retrieve the slice,
/// then the caller iterates the (typically small) slice. Empty when the
/// token has no match.
#[derive(Debug, Clone, Default)]
pub struct SchemaVocabulary {
    inverted: HashMap<String, Vec<VocabHit>>,
}

impl SchemaVocabulary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lookup a token. The caller is expected to pass the *raw* token;
    /// we normalise before probing so callers don't have to remember
    /// the rule.
    pub fn lookup(&self, token: &str) -> &[VocabHit] {
        let key = match normalise(token) {
            Some(k) => k,
            None => return &[],
        };
        self.inverted
            .get(&key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Apply a DDL event to the index.
    pub fn on_ddl(&mut self, event: DdlEvent) {
        match event {
            DdlEvent::CreateCollection {
                collection,
                columns,
                type_tags,
                description,
            } => {
                self.insert_collection(&collection, &columns, &type_tags, description.as_deref());
            }
            DdlEvent::AlterCollection {
                collection,
                columns,
                type_tags,
                description,
            } => {
                // Drop+recreate is the simplest way to keep
                // invalidation complete: stale columns / discriminators
                // never linger after an ALTER ... DROP COLUMN.
                self.purge_collection_entries(&collection);
                self.insert_collection(&collection, &columns, &type_tags, description.as_deref());
            }
            DdlEvent::DropCollection { collection } => {
                self.purge_collection_entries(&collection);
            }
            DdlEvent::CreateIndex {
                collection,
                index,
                columns,
            } => {
                self.insert_index(&collection, &index, &columns);
            }
            DdlEvent::DropIndex { collection, index } => {
                self.remove_token_for(&index, &collection, |hit| hit.column.is_none());
            }
            DdlEvent::CreatePolicy { collection, policy } => {
                self.insert_token(
                    &policy,
                    VocabHit {
                        collection: collection.clone(),
                        column: None,
                        type_tag: Some(format!("policy:{}", policy)),
                    },
                );
            }
            DdlEvent::DropPolicy { collection, policy } => {
                let tag = format!("policy:{}", policy);
                self.remove_token_for(&policy, &collection, |hit| {
                    hit.type_tag.as_deref() == Some(tag.as_str())
                });
            }
        }
    }

    fn insert_collection(
        &mut self,
        collection: &str,
        columns: &[ColumnName],
        type_tags: &[String],
        description: Option<&str>,
    ) {
        // Collection-name-only hit.
        self.insert_token(
            collection,
            VocabHit {
                collection: collection.to_string(),
                column: None,
                type_tag: None,
            },
        );
        // One hit per column.
        for column in columns {
            self.insert_token(
                column,
                VocabHit {
                    collection: collection.to_string(),
                    column: Some(column.clone()),
                    type_tag: None,
                },
            );
        }
        // One hit per type tag, attached to the collection (no column).
        for tag in type_tags {
            self.insert_token(
                tag,
                VocabHit {
                    collection: collection.to_string(),
                    column: None,
                    type_tag: Some(tag.clone()),
                },
            );
        }
        // Description tokens fan into individual word entries.
        if let Some(text) = description {
            for word in tokenise_description(text) {
                self.insert_token(
                    &word,
                    VocabHit {
                        collection: collection.to_string(),
                        column: None,
                        type_tag: Some("description".to_string()),
                    },
                );
            }
        }
    }

    fn insert_index(&mut self, collection: &str, index: &str, columns: &[ColumnName]) {
        self.insert_token(
            index,
            VocabHit {
                collection: collection.to_string(),
                column: None,
                type_tag: Some(format!("index:{}", index)),
            },
        );
        for column in columns {
            // Surface the column too so a token match still leads back
            // to the collection even if the column wasn't declared in
            // the CollectionContract (e.g. CREATE INDEX over a dynamic
            // doc shape).
            self.insert_token(
                column,
                VocabHit {
                    collection: collection.to_string(),
                    column: Some(column.clone()),
                    type_tag: Some(format!("index:{}", index)),
                },
            );
        }
    }

    fn insert_token(&mut self, raw: &str, hit: VocabHit) {
        let Some(key) = normalise(raw) else { return };
        let bucket = self.inverted.entry(key).or_default();
        if !bucket.iter().any(|existing| existing == &hit) {
            bucket.push(hit);
        }
    }

    /// Remove every entry whose `collection` matches. Empty buckets are
    /// pruned so the HashMap stays compact.
    fn purge_collection_entries(&mut self, collection: &str) {
        let mut empty_keys: Vec<String> = Vec::new();
        for (key, bucket) in self.inverted.iter_mut() {
            bucket.retain(|hit| hit.collection != collection);
            if bucket.is_empty() {
                empty_keys.push(key.clone());
            }
        }
        for key in empty_keys {
            self.inverted.remove(&key);
        }
    }

    /// Remove the entries under one token whose hit predicate matches
    /// for the given collection.
    fn remove_token_for<F>(&mut self, raw: &str, collection: &str, predicate: F)
    where
        F: Fn(&VocabHit) -> bool,
    {
        let Some(key) = normalise(raw) else { return };
        let Some(bucket) = self.inverted.get_mut(&key) else {
            return;
        };
        bucket.retain(|hit| !(hit.collection == collection && predicate(hit)));
        if bucket.is_empty() {
            self.inverted.remove(&key);
        }
    }

    /// Number of distinct tokens currently indexed. Test helper.
    #[cfg(test)]
    pub(crate) fn token_count(&self) -> usize {
        self.inverted.len()
    }

    /// Distinct collections covered. Test helper.
    #[cfg(test)]
    pub(crate) fn collections(&self) -> HashSet<String> {
        self.inverted
            .values()
            .flat_map(|bucket| bucket.iter().map(|hit| hit.collection.clone()))
            .collect()
    }
}

// `HashSet` is used by test-only helpers; the import has to stay
// outside the `cfg(test)` block so doc-builds do not trip an unused
// import warning when tests are off.
#[allow(dead_code)]
fn _force_hashset_use(_: HashSet<String>) {}

/// Normalise a raw token to its index key. Returns `None` when the
/// resulting string would be empty (e.g. caller passed `"---"`).
pub fn normalise(raw: &str) -> Option<String> {
    let mut buf = String::with_capacity(raw.len());
    for ch in nfkd_decompose(raw) {
        // Strip Unicode combining marks (accents / diacritics). The
        // common Latin set we care about (`U+0300..=U+036F`) covers
        // every diacritic that the issue's test corpus exercises;
        // wider blocks (`U+1AB0..U+1AFF`, `U+1DC0..U+1DFF`,
        // `U+20D0..U+20FF`, `U+FE20..U+FE2F`) are also stripped so
        // exotic inputs don't smuggle invisible marks through.
        if is_combining_mark(ch) {
            continue;
        }
        buf.push(ch);
    }

    let lowered = buf.to_lowercase();
    let trimmed = lowered.trim_matches(|c: char| !c.is_alphanumeric());
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Lazy NFKD decomposition without pulling a new crate. The runtime
/// already lives without `unicode-normalization`, so we hand-roll a
/// minimal pre-composed-Latin folder. It covers every accented letter
/// the issue's `passaporte`/`pässäpörte` test exercises plus the
/// common European diacritics. Anything outside the table passes
/// through unchanged.
fn nfkd_decompose(input: &str) -> impl Iterator<Item = char> + '_ {
    input.chars().flat_map(decompose_char)
}

fn decompose_char(ch: char) -> Vec<char> {
    // Map of pre-composed → (base, combining mark). Only the marks we
    // need for accent-folding common European text. Anything else is
    // emitted verbatim.
    let pair: Option<(char, char)> = match ch {
        // Lowercase a
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => Some((
            'a',
            match ch {
                'à' => '\u{0300}',
                'á' => '\u{0301}',
                'â' => '\u{0302}',
                'ã' => '\u{0303}',
                'ä' => '\u{0308}',
                'å' => '\u{030A}',
                _ => unreachable!(),
            },
        )),
        // Uppercase A
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => Some((
            'A',
            match ch {
                'À' => '\u{0300}',
                'Á' => '\u{0301}',
                'Â' => '\u{0302}',
                'Ã' => '\u{0303}',
                'Ä' => '\u{0308}',
                'Å' => '\u{030A}',
                _ => unreachable!(),
            },
        )),
        // Lowercase e
        'è' | 'é' | 'ê' | 'ë' => Some((
            'e',
            match ch {
                'è' => '\u{0300}',
                'é' => '\u{0301}',
                'ê' => '\u{0302}',
                'ë' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'È' | 'É' | 'Ê' | 'Ë' => Some((
            'E',
            match ch {
                'È' => '\u{0300}',
                'É' => '\u{0301}',
                'Ê' => '\u{0302}',
                'Ë' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'ì' | 'í' | 'î' | 'ï' => Some((
            'i',
            match ch {
                'ì' => '\u{0300}',
                'í' => '\u{0301}',
                'î' => '\u{0302}',
                'ï' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'Ì' | 'Í' | 'Î' | 'Ï' => Some((
            'I',
            match ch {
                'Ì' => '\u{0300}',
                'Í' => '\u{0301}',
                'Î' => '\u{0302}',
                'Ï' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => Some((
            'o',
            match ch {
                'ò' => '\u{0300}',
                'ó' => '\u{0301}',
                'ô' => '\u{0302}',
                'õ' => '\u{0303}',
                'ö' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => Some((
            'O',
            match ch {
                'Ò' => '\u{0300}',
                'Ó' => '\u{0301}',
                'Ô' => '\u{0302}',
                'Õ' => '\u{0303}',
                'Ö' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'ù' | 'ú' | 'û' | 'ü' => Some((
            'u',
            match ch {
                'ù' => '\u{0300}',
                'ú' => '\u{0301}',
                'û' => '\u{0302}',
                'ü' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'Ù' | 'Ú' | 'Û' | 'Ü' => Some((
            'U',
            match ch {
                'Ù' => '\u{0300}',
                'Ú' => '\u{0301}',
                'Û' => '\u{0302}',
                'Ü' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'ñ' => Some(('n', '\u{0303}')),
        'Ñ' => Some(('N', '\u{0303}')),
        'ç' => Some(('c', '\u{0327}')),
        'Ç' => Some(('C', '\u{0327}')),
        'ý' | 'ÿ' => Some((
            'y',
            match ch {
                'ý' => '\u{0301}',
                'ÿ' => '\u{0308}',
                _ => unreachable!(),
            },
        )),
        'Ý' => Some(('Y', '\u{0301}')),
        _ => None,
    };
    match pair {
        Some((base, mark)) => vec![base, mark],
        None => vec![ch],
    }
}

fn is_combining_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F)
}

/// Split a description into normalised word tokens.
fn tokenise_description(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(normalise)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_event(name: &str, columns: &[&str]) -> DdlEvent {
        DdlEvent::CreateCollection {
            collection: name.to_string(),
            columns: columns.iter().map(|s| s.to_string()).collect(),
            type_tags: Vec::new(),
            description: None,
        }
    }

    #[test]
    fn normalisation_lowercases() {
        assert_eq!(normalise("PASSPORT").as_deref(), Some("passport"));
        assert_eq!(normalise("Passport").as_deref(), Some("passport"));
    }

    #[test]
    fn normalisation_strips_accents() {
        // Pin: passport ≡ PASSPORT ≡ pässpört.
        let lowered = normalise("passport").unwrap();
        let upper = normalise("PASSPORT").unwrap();
        let accented = normalise("pässpört").unwrap();
        assert_eq!(lowered, "passport");
        assert_eq!(upper, "passport");
        assert_eq!(accented, "passport");
        // Portuguese passaporte vs PASSAPORTE vs pässäpörte all collapse.
        let pt = normalise("passaporte").unwrap();
        let pt_upper = normalise("PASSAPORTE").unwrap();
        let pt_accented = normalise("pässäpörte").unwrap();
        assert_eq!(pt, "passaporte");
        assert_eq!(pt_upper, "passaporte");
        assert_eq!(pt_accented, "passaporte");
    }

    #[test]
    fn normalisation_strips_surrounding_punctuation() {
        assert_eq!(normalise("---email---").as_deref(), Some("email"));
        assert_eq!(normalise("?id?").as_deref(), Some("id"));
        // Inner punctuation is preserved (single-token rule).
        assert_eq!(normalise("snake_case").as_deref(), Some("snake_case"));
    }

    #[test]
    fn normalisation_returns_none_for_empty_or_punct_only() {
        assert!(normalise("").is_none());
        assert!(normalise("---").is_none());
        assert!(normalise("   ").is_none());
    }

    #[test]
    fn lookup_finds_collection_name_hit() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("passports", &["id", "country"]));
        let hits = vocab.lookup("passports");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].collection, "passports");
        assert!(hits[0].column.is_none());
    }

    #[test]
    fn lookup_finds_column_hits_across_collections() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("users", &["id", "email"]));
        vocab.on_ddl(create_event("orders", &["id", "user_id"]));
        let id_hits = vocab.lookup("id");
        let collections: HashSet<_> =
            id_hits.iter().map(|h| h.collection.as_str()).collect();
        assert!(collections.contains("users"));
        assert!(collections.contains("orders"));
        assert!(id_hits.iter().all(|h| h.column.as_deref() == Some("id")));
    }

    #[test]
    fn lookup_is_accent_fold_aware() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("passaporte", &["id"]));
        // Hits arrive even when the caller passes a different
        // accent / case form than the one the DDL declared.
        let via_accents = vocab.lookup("PÄSSÄPÖRTE");
        assert_eq!(via_accents.len(), 1);
        assert_eq!(via_accents[0].collection, "passaporte");
    }

    #[test]
    fn drop_collection_invalidates_completely() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("users", &["id", "email"]));
        vocab.on_ddl(create_event("orders", &["id"]));
        assert!(!vocab.lookup("users").is_empty());
        vocab.on_ddl(DdlEvent::DropCollection {
            collection: "users".to_string(),
        });
        // No stale entries.
        assert!(vocab.lookup("users").is_empty());
        assert!(vocab.lookup("email").is_empty());
        // Other collection still resolves.
        let id_hits = vocab.lookup("id");
        assert_eq!(id_hits.len(), 1);
        assert_eq!(id_hits[0].collection, "orders");
    }

    #[test]
    fn alter_collection_replaces_column_set() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("users", &["id", "email"]));
        vocab.on_ddl(DdlEvent::AlterCollection {
            collection: "users".to_string(),
            columns: vec!["id".to_string(), "username".to_string()],
            type_tags: Vec::new(),
            description: None,
        });
        // Old column dropped.
        assert!(vocab.lookup("email").is_empty());
        // New column visible.
        let username_hits = vocab.lookup("username");
        assert_eq!(username_hits.len(), 1);
        assert_eq!(username_hits[0].column.as_deref(), Some("username"));
    }

    #[test]
    fn index_create_and_drop_round_trip() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("users", &["email"]));
        vocab.on_ddl(DdlEvent::CreateIndex {
            collection: "users".to_string(),
            index: "idx_users_email".to_string(),
            columns: vec!["email".to_string()],
        });
        let idx_hits = vocab.lookup("idx_users_email");
        assert_eq!(idx_hits.len(), 1);
        assert!(idx_hits[0]
            .type_tag
            .as_deref()
            .map(|t| t.starts_with("index:"))
            .unwrap_or(false));
        vocab.on_ddl(DdlEvent::DropIndex {
            collection: "users".to_string(),
            index: "idx_users_email".to_string(),
        });
        assert!(vocab.lookup("idx_users_email").is_empty());
    }

    #[test]
    fn policy_create_and_drop_round_trip() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(create_event("users", &["id"]));
        vocab.on_ddl(DdlEvent::CreatePolicy {
            collection: "users".to_string(),
            policy: "tenant_isolation".to_string(),
        });
        let hits = vocab.lookup("tenant_isolation");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].collection, "users");
        vocab.on_ddl(DdlEvent::DropPolicy {
            collection: "users".to_string(),
            policy: "tenant_isolation".to_string(),
        });
        assert!(vocab.lookup("tenant_isolation").is_empty());
    }

    #[test]
    fn type_tags_resolve_doc_shape_discriminators() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(DdlEvent::CreateCollection {
            collection: "events".to_string(),
            columns: vec!["id".to_string(), "type".to_string()],
            type_tags: vec!["payment".to_string(), "refund".to_string()],
            description: None,
        });
        let hits = vocab.lookup("payment");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].collection, "events");
        assert_eq!(hits[0].type_tag.as_deref(), Some("payment"));
    }

    #[test]
    fn description_words_resolve_to_collection() {
        let mut vocab = SchemaVocabulary::new();
        vocab.on_ddl(DdlEvent::CreateCollection {
            collection: "users".to_string(),
            columns: vec!["id".to_string()],
            type_tags: Vec::new(),
            description: Some("Customer accounts and sign-in info".to_string()),
        });
        let hits = vocab.lookup("customer");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].collection, "users");
    }

    // -- Property test (issue #120 acceptance row) --
    //
    // For 256 random (schema, query) pairs: every token the lookup
    // returns must point at a collection that *actually* contains the
    // referenced column. No phantom hits.
    use proptest::prelude::*;

    fn ascii_ident() -> impl Strategy<Value = String> {
        // Compact alphabetic identifier so the property test stays
        // deterministic and bounded.
        "[a-z][a-z0-9_]{0,8}".prop_map(|s| s)
    }

    fn schema_strategy() -> impl Strategy<Value = Vec<(String, Vec<String>)>> {
        prop::collection::vec(
            (
                ascii_ident(),
                prop::collection::vec(ascii_ident(), 0..6),
            ),
            1..6,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            .. ProptestConfig::default()
        })]
        #[test]
        fn property_every_hit_points_at_real_column(
            schema in schema_strategy(),
            query_tokens in prop::collection::vec(ascii_ident(), 1..8),
        ) {
            // Build the fixture from the random schema. Disambiguate
            // collection names so two CREATE TABLE events on the same
            // name don't double-count.
            let mut seen = HashSet::new();
            let mut deduped: Vec<(String, Vec<String>)> = Vec::new();
            for (collection, columns) in schema {
                if seen.insert(collection.clone()) {
                    let mut cols_seen = HashSet::new();
                    let cols: Vec<String> = columns
                        .into_iter()
                        .filter(|c| cols_seen.insert(c.clone()))
                        .collect();
                    deduped.push((collection, cols));
                }
            }
            let mut vocab = SchemaVocabulary::new();
            for (collection, columns) in &deduped {
                vocab.on_ddl(DdlEvent::CreateCollection {
                    collection: collection.clone(),
                    columns: columns.clone(),
                    type_tags: Vec::new(),
                    description: None,
                });
            }
            // Index for verification.
            let by_collection: HashMap<String, HashSet<String>> = deduped
                .iter()
                .map(|(c, cols)| (c.clone(), cols.iter().cloned().collect()))
                .collect();
            for token in query_tokens {
                let hits = vocab.lookup(&token);
                for hit in hits {
                    let cols = by_collection.get(&hit.collection).expect(
                        "lookup returned a collection that was never created",
                    );
                    match &hit.column {
                        Some(column) => {
                            prop_assert!(
                                cols.contains(column),
                                "phantom column {} on {}",
                                column,
                                hit.collection
                            );
                        }
                        None => {
                            // Collection-name hit: token must normalise
                            // to the collection name itself (or be a
                            // type_tag, but the schema strategy doesn't
                            // emit type_tags).
                            let normalised_collection = normalise(&hit.collection)
                                .unwrap_or_default();
                            let normalised_token = normalise(&token).unwrap_or_default();
                            prop_assert_eq!(normalised_collection, normalised_token);
                        }
                    }
                }
            }
        }
    }
}
