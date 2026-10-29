//! Update a derived full-text frame from a bounded set of changed entity IDs.
use std::collections::{BTreeMap, HashSet};

use reddb_file::{NativeFulltextFrame, NativeFulltextPosting, NativeFulltextTerm};

pub(super) fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_owned)
        .collect()
}

/// `complete_document_ids` proves that every previously counted document had
/// at least one posting. Frames count nonempty single-character documents too,
/// so the number of removed documents cannot otherwise be inferred from terms.
/// The caller must separately prove the source manager, revisions and pages.
pub(super) fn apply_delta(
    frame: &mut NativeFulltextFrame,
    changes: &BTreeMap<u64, Option<String>>,
    complete_document_ids: bool,
) -> Option<bool> {
    if !complete_document_ids {
        return None;
    }
    let changed: HashSet<_> = changes.keys().copied().collect();
    let mut removed = HashSet::new();
    for term in &mut frame.terms {
        term.postings.retain(|posting| {
            if changed.contains(&posting.entity_id) {
                removed.insert(posting.entity_id);
                false
            } else {
                true
            }
        });
    }
    let mut added = 0_u32;
    let mut complete = true;
    let mut additions: BTreeMap<String, Vec<NativeFulltextPosting>> = BTreeMap::new();
    for (&entity_id, text) in changes {
        let Some(text) = text.as_ref().filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        added = added.checked_add(1)?;
        let mut frequencies = BTreeMap::<String, u32>::new();
        for token in tokenize(text) {
            *frequencies.entry(token).or_default() += 1;
        }
        complete &= !frequencies.is_empty();
        for (term, term_count) in frequencies {
            additions
                .entry(term)
                .or_default()
                .push(NativeFulltextPosting {
                    entity_id,
                    term_count,
                });
        }
    }
    frame.doc_count = frame
        .doc_count
        .checked_sub(u32::try_from(removed.len()).ok()?)?
        .checked_add(added)?;

    // Move existing strings and posting lists; do not duplicate the old index.
    // Both the full builder and this path must serialize postings by entity ID.
    let old_terms = std::mem::take(&mut frame.terms);
    let mut terms = Vec::with_capacity(old_terms.len() + additions.len());
    for mut old in old_terms {
        while additions
            .first_key_value()
            .is_some_and(|(term, _)| term < &old.term)
        {
            let (term, postings) = additions.pop_first().expect("first term exists");
            terms.push(NativeFulltextTerm { term, postings });
        }
        if let Some(postings) = additions.remove(&old.term) {
            old.postings.extend(postings);
        }
        if !old.postings.is_empty() {
            old.postings
                .sort_unstable_by_key(|posting| posting.entity_id);
            terms.push(old);
        }
    }
    terms.extend(
        additions
            .into_iter()
            .map(|(term, postings)| NativeFulltextTerm { term, postings }),
    );
    frame.terms = terms;
    Some(complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(documents: &BTreeMap<u64, Option<String>>) -> NativeFulltextFrame {
        let mut terms: BTreeMap<String, Vec<NativeFulltextPosting>> = BTreeMap::new();
        let mut doc_count = 0;
        for (&entity_id, text) in documents {
            let Some(text) = text.as_ref().filter(|text| !text.trim().is_empty()) else {
                continue;
            };
            doc_count += 1;
            let mut frequencies = BTreeMap::<String, u32>::new();
            for token in text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|token| token.len() >= 2)
            {
                *frequencies.entry(token.to_owned()).or_default() += 1;
            }
            for (term, term_count) in frequencies {
                terms.entry(term).or_default().push(NativeFulltextPosting {
                    entity_id,
                    term_count,
                });
            }
        }
        NativeFulltextFrame {
            collection: "rows".to_owned(),
            doc_count,
            terms: terms
                .into_iter()
                .map(|(term, postings)| NativeFulltextTerm { term, postings })
                .collect(),
        }
    }

    #[test]
    fn append_edit_delete_and_unicode_match_a_complete_rebuild() {
        let mut documents: BTreeMap<_, _> = [
            (30, Some("Alpha alpha café 東京".to_owned())),
            (4, Some("beta CAFÉ".to_owned())),
            (200, Some("to remove".to_owned())),
        ]
        .into_iter()
        .collect();
        let mut frame = reference(&documents);
        let changes: BTreeMap<_, _> = [
            (30, Some("changed alpha 東京 東京".to_owned())),
            (200, None),
            (1, Some("first CAFÉ".to_owned())),
            (1000, Some("last alpha".to_owned())),
        ]
        .into_iter()
        .collect();
        assert_eq!(apply_delta(&mut frame, &changes, true), Some(true));
        documents.extend(changes);
        assert_eq!(
            reddb_file::encode_native_fulltext_frame(&frame),
            reddb_file::encode_native_fulltext_frame(&reference(&documents))
        );
    }

    #[test]
    fn zero_posting_documents_require_a_full_rebuild_on_the_next_change() {
        let documents = [(1, Some("alpha".to_owned()))].into_iter().collect();
        let mut frame = reference(&documents);
        let changes = [(1, Some("a".to_owned()))].into_iter().collect();
        assert_eq!(apply_delta(&mut frame, &changes, true), Some(false));
        assert_eq!(frame.doc_count, 1);
        assert!(frame.terms.is_empty());
        let before = reddb_file::encode_native_fulltext_frame(&frame);
        assert_eq!(apply_delta(&mut frame, &changes, false), None);
        assert_eq!(before, reddb_file::encode_native_fulltext_frame(&frame));
    }

    #[test]
    fn deleting_one_document_decrements_count_once_across_all_its_terms() {
        let documents = [(1, Some("alpha beta gamma".to_owned()))]
            .into_iter()
            .collect();
        let mut frame = reference(&documents);
        assert_eq!(
            apply_delta(&mut frame, &[(1, None)].into_iter().collect(), true),
            Some(true)
        );
        assert_eq!(frame.doc_count, 0);
        assert!(frame.terms.is_empty());
    }
}
