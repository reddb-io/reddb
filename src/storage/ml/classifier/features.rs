//! Light-weight feature extractors.
//!
//! Production-grade feature engineering lives outside this module
//! (callers can plug arbitrary vectors into the classifier). The
//! helpers here exist so the classifier surface tests can exercise
//! TF-IDF-style vectors end-to-end without dragging tokeniser
//! dependencies in.

use std::collections::HashMap;

/// Shared vocabulary learnt from a corpus. Incremental — new tokens
/// allocate a fresh index on `add`.
#[derive(Debug, Default, Clone)]
pub struct Vocabulary {
    index: HashMap<String, usize>,
    document_frequency: Vec<u64>,
    total_documents: u64,
}

impl Vocabulary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a document — increments document frequency for each
    /// distinct token observed.
    pub fn add_document(&mut self, tokens: &[&str]) {
        let mut seen: HashMap<String, ()> = HashMap::new();
        for t in tokens {
            let key = t.to_ascii_lowercase();
            if seen.contains_key(&key) {
                continue;
            }
            let idx = match self.index.get(&key) {
                Some(i) => *i,
                None => {
                    let i = self.document_frequency.len();
                    self.index.insert(key.clone(), i);
                    self.document_frequency.push(0);
                    i
                }
            };
            self.document_frequency[idx] += 1;
            seen.insert(key, ());
        }
        self.total_documents += 1;
    }

    pub fn dimensions(&self) -> usize {
        self.document_frequency.len()
    }

    pub fn index_of(&self, token: &str) -> Option<usize> {
        self.index.get(&token.to_ascii_lowercase()).copied()
    }
}

/// Build a TF-IDF vector of length `vocab.dimensions()` from a single
/// document's tokens. Tokens not in the vocabulary are ignored
/// (caller should have called `vocab.add_document` during training).
pub fn tf_idf_vectorize(vocab: &Vocabulary, tokens: &[&str]) -> Vec<f32> {
    if vocab.dimensions() == 0 {
        return Vec::new();
    }
    let mut tf = vec![0f32; vocab.dimensions()];
    let mut total = 0f32;
    for t in tokens {
        if let Some(idx) = vocab.index_of(t) {
            tf[idx] += 1.0;
            total += 1.0;
        }
    }
    if total > 0.0 {
        for v in tf.iter_mut() {
            *v /= total;
        }
    }
    let total_docs = (vocab.total_documents.max(1)) as f32;
    for i in 0..vocab.dimensions() {
        let df = vocab.document_frequency[i].max(1) as f32;
        let idf = ((total_docs + 1.0) / (df + 1.0)).ln() + 1.0;
        tf[i] *= idf;
    }
    tf
}

/// Build a one-hot vector of length `num_classes` with 1.0 at
/// `class` and 0.0 elsewhere. Returns an empty vector if `class`
/// is out of range.
pub fn one_hot(class: u32, num_classes: usize) -> Vec<f32> {
    let c = class as usize;
    if c >= num_classes {
        return Vec::new();
    }
    let mut v = vec![0f32; num_classes];
    v[c] = 1.0;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_allocates_indices_incrementally() {
        let mut v = Vocabulary::new();
        v.add_document(&["cat", "dog"]);
        v.add_document(&["dog", "bird"]);
        assert_eq!(v.dimensions(), 3);
        assert!(v.index_of("cat").is_some());
        assert!(v.index_of("dog").is_some());
        assert!(v.index_of("bird").is_some());
        // DF: cat 1, dog 2, bird 1
    }

    #[test]
    fn tf_idf_vectorises_in_vocabulary_tokens() {
        let mut v = Vocabulary::new();
        v.add_document(&["cat", "cat", "the"]);
        v.add_document(&["dog", "the"]);
        v.add_document(&["cat", "dog"]);
        let vec = tf_idf_vectorize(&v, &["cat"]);
        assert_eq!(vec.len(), v.dimensions());
        assert!(vec[v.index_of("cat").unwrap()] > 0.0);
        assert_eq!(vec[v.index_of("dog").unwrap()], 0.0);
    }

    #[test]
    fn tf_idf_ignores_oov_tokens() {
        let mut v = Vocabulary::new();
        v.add_document(&["hello"]);
        let vec = tf_idf_vectorize(&v, &["nope", "missing"]);
        for x in vec {
            assert_eq!(x, 0.0);
        }
    }

    #[test]
    fn one_hot_is_correct_length_and_position() {
        let v = one_hot(2, 4);
        assert_eq!(v, vec![0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn one_hot_rejects_out_of_range_class() {
        assert!(one_hot(5, 3).is_empty());
    }
}
