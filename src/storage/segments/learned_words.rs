//! Learned Words Segment - Persistence for TF-IDF learned vocabulary.
//!
//! Stores words learned during scanning with their TF-IDF scores,
//! domain associations, and context information.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::primitives::encoding::{
    read_f64, read_string, read_varu32, read_varu64, write_f64, write_string, write_varu32,
    write_varu64, DecodeError,
};

// ==================== Learned Word ====================

/// A word learned from scanning with metadata
#[derive(Debug, Clone)]
pub struct LearnedWordEntry {
    /// The word itself
    pub word: String,
    /// TF-IDF score (0.0 - 1.0+)
    pub tf_idf_score: f64,
    /// Number of documents containing this word
    pub document_count: u32,
    /// Total occurrences across all documents
    pub total_occurrences: u32,
    /// Unix timestamp of first discovery (ms)
    pub first_seen: u64,
    /// Unix timestamp of last occurrence (ms)
    pub last_seen: u64,
    /// Source context (directories, subdomains, etc.)
    pub context: String,
}

impl LearnedWordEntry {
    pub fn new(word: String, context: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            word,
            tf_idf_score: 0.0,
            document_count: 1,
            total_occurrences: 1,
            first_seen: now,
            last_seen: now,
            context,
        }
    }

    pub fn with_score(mut self, score: f64) -> Self {
        self.tf_idf_score = score;
        self
    }

    /// Update statistics when word is seen again
    pub fn update(&mut self, new_score: f64) {
        self.last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.document_count += 1;
        self.total_occurrences += 1;
        // Update score with exponential moving average
        self.tf_idf_score = self.tf_idf_score * 0.7 + new_score * 0.3;
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.word);
        write_f64(buf, self.tf_idf_score);
        write_varu32(buf, self.document_count);
        write_varu32(buf, self.total_occurrences);
        write_varu64(buf, self.first_seen);
        write_varu64(buf, self.last_seen);
        write_string(buf, &self.context);
    }

    fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let word = read_string(bytes, pos)?.to_string();
        let tf_idf_score = read_f64(bytes, pos)?;
        let document_count = read_varu32(bytes, pos)?;
        let total_occurrences = read_varu32(bytes, pos)?;
        let first_seen = read_varu64(bytes, pos)?;
        let last_seen = read_varu64(bytes, pos)?;
        let context = read_string(bytes, pos)?.to_string();

        Ok(Self {
            word,
            tf_idf_score,
            document_count,
            total_occurrences,
            first_seen,
            last_seen,
            context,
        })
    }
}

// ==================== Domain Vocabulary ====================

/// Vocabulary learned for a specific domain
#[derive(Debug, Clone, Default)]
pub struct DomainVocabulary {
    /// Domain name (e.g., "example.com")
    pub domain: String,
    /// Words learned for this domain
    pub words: HashMap<String, LearnedWordEntry>,
    /// Total documents processed
    pub total_documents: u32,
    /// First scan timestamp
    pub first_scan: u64,
    /// Last scan timestamp
    pub last_scan: u64,
}

impl DomainVocabulary {
    pub fn new(domain: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            domain,
            words: HashMap::new(),
            total_documents: 0,
            first_scan: now,
            last_scan: now,
        }
    }

    /// Add or update a word
    pub fn add_word(&mut self, word: String, context: &str, score: f64) {
        if let Some(entry) = self.words.get_mut(&word) {
            entry.update(score);
        } else {
            let entry = LearnedWordEntry::new(word.clone(), context.to_string()).with_score(score);
            self.words.insert(word, entry);
        }
    }

    /// Record document processing
    pub fn record_document(&mut self) {
        self.total_documents += 1;
        self.last_scan = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Get top N words by TF-IDF score
    pub fn top_words(&self, n: usize) -> Vec<&LearnedWordEntry> {
        let mut words: Vec<_> = self.words.values().collect();
        words.sort_by(|a, b| {
            b.tf_idf_score
                .partial_cmp(&a.tf_idf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        words.truncate(n);
        words
    }

    /// Get words for a specific context
    pub fn words_for_context(&self, context: &str) -> Vec<&LearnedWordEntry> {
        self.words
            .values()
            .filter(|w| w.context == context)
            .collect()
    }

    /// Prune vocabulary to max size, keeping highest scoring words
    pub fn prune(&mut self, max_size: usize) {
        if self.words.len() <= max_size {
            return;
        }

        // Get words sorted by score
        let mut word_scores: Vec<_> = self
            .words
            .iter()
            .map(|(k, v)| (k.clone(), v.tf_idf_score))
            .collect();
        word_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Keep only top N
        let to_keep: std::collections::HashSet<_> = word_scores
            .into_iter()
            .take(max_size)
            .map(|(k, _)| k)
            .collect();

        self.words.retain(|k, _| to_keep.contains(k));
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.domain);
        write_varu32(buf, self.total_documents);
        write_varu64(buf, self.first_scan);
        write_varu64(buf, self.last_scan);
        write_varu32(buf, self.words.len() as u32);
        for entry in self.words.values() {
            entry.encode(buf);
        }
    }

    fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let domain = read_string(bytes, pos)?.to_string();
        let total_documents = read_varu32(bytes, pos)?;
        let first_scan = read_varu64(bytes, pos)?;
        let last_scan = read_varu64(bytes, pos)?;
        let word_count = read_varu32(bytes, pos)? as usize;

        let mut words = HashMap::with_capacity(word_count);
        for _ in 0..word_count {
            let entry = LearnedWordEntry::decode(bytes, pos)?;
            words.insert(entry.word.clone(), entry);
        }

        Ok(Self {
            domain,
            words,
            total_documents,
            first_scan,
            last_scan,
        })
    }
}

// ==================== Global Vocabulary ====================

/// Global vocabulary (not domain-specific)
#[derive(Debug, Clone, Default)]
pub struct GlobalVocabulary {
    /// Words by context
    pub contexts: HashMap<String, HashMap<String, LearnedWordEntry>>,
    /// Total documents processed
    pub total_documents: u32,
}

impl GlobalVocabulary {
    pub fn new() -> Self {
        Self {
            contexts: HashMap::new(),
            total_documents: 0,
        }
    }

    /// Add word to global vocabulary
    pub fn add_word(&mut self, word: String, context: &str, score: f64) {
        let context_words = self.contexts.entry(context.to_string()).or_default();

        if let Some(entry) = context_words.get_mut(&word) {
            entry.update(score);
        } else {
            let entry = LearnedWordEntry::new(word.clone(), context.to_string()).with_score(score);
            context_words.insert(word, entry);
        }
    }

    /// Get top words for a context
    pub fn top_words_for_context(&self, context: &str, n: usize) -> Vec<&LearnedWordEntry> {
        self.contexts
            .get(context)
            .map(|words| {
                let mut entries: Vec<_> = words.values().collect();
                entries.sort_by(|a, b| {
                    b.tf_idf_score
                        .partial_cmp(&a.tf_idf_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                entries.truncate(n);
                entries
            })
            .unwrap_or_default()
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        write_varu32(buf, self.total_documents);
        write_varu32(buf, self.contexts.len() as u32);
        for (context, words) in &self.contexts {
            write_string(buf, context);
            write_varu32(buf, words.len() as u32);
            for entry in words.values() {
                entry.encode(buf);
            }
        }
    }

    fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let total_documents = read_varu32(bytes, pos)?;
        let context_count = read_varu32(bytes, pos)? as usize;

        let mut contexts = HashMap::with_capacity(context_count);
        for _ in 0..context_count {
            let context = read_string(bytes, pos)?.to_string();
            let word_count = read_varu32(bytes, pos)? as usize;

            let mut words = HashMap::with_capacity(word_count);
            for _ in 0..word_count {
                let entry = LearnedWordEntry::decode(bytes, pos)?;
                words.insert(entry.word.clone(), entry);
            }

            contexts.insert(context, words);
        }

        Ok(Self {
            contexts,
            total_documents,
        })
    }
}

// ==================== Learned Words Segment ====================

/// Storage segment for learned vocabulary
#[derive(Debug, Clone, Default)]
pub struct LearnedWordsSegment {
    /// Per-domain vocabularies
    pub domains: HashMap<String, DomainVocabulary>,
    /// Global vocabulary (cross-domain)
    pub global: GlobalVocabulary,
    /// Maximum words per domain
    pub max_words_per_domain: usize,
    /// Maximum words in global vocabulary per context
    pub max_global_words_per_context: usize,
}

impl LearnedWordsSegment {
    pub fn new() -> Self {
        Self {
            domains: HashMap::new(),
            global: GlobalVocabulary::new(),
            max_words_per_domain: 10_000,
            max_global_words_per_context: 50_000,
        }
    }

    /// Get or create domain vocabulary
    pub fn get_or_create_domain(&mut self, domain: &str) -> &mut DomainVocabulary {
        self.domains
            .entry(domain.to_string())
            .or_insert_with(|| DomainVocabulary::new(domain.to_string()))
    }

    /// Get domain vocabulary
    pub fn get_domain(&self, domain: &str) -> Option<&DomainVocabulary> {
        self.domains.get(domain)
    }

    /// Add word for domain and context
    pub fn add_word(&mut self, domain: &str, word: String, context: &str, score: f64) {
        // Add to domain vocabulary
        let vocab = self.get_or_create_domain(domain);
        vocab.add_word(word.clone(), context, score);

        // Also add to global
        self.global.add_word(word, context, score);
    }

    /// Record document processing for domain
    pub fn record_document(&mut self, domain: &str) {
        let vocab = self.get_or_create_domain(domain);
        vocab.record_document();
        self.global.total_documents += 1;
    }

    /// Get top words for domain and context
    pub fn top_words_for_domain(
        &self,
        domain: &str,
        context: &str,
        n: usize,
    ) -> Vec<&LearnedWordEntry> {
        self.domains
            .get(domain)
            .map(|v| {
                let mut words = v.words_for_context(context);
                words.sort_by(|a, b| {
                    b.tf_idf_score
                        .partial_cmp(&a.tf_idf_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                words.truncate(n);
                words
            })
            .unwrap_or_default()
    }

    /// Get top global words for context
    pub fn top_global_words(&self, context: &str, n: usize) -> Vec<&LearnedWordEntry> {
        self.global.top_words_for_context(context, n)
    }

    /// Merge learned words from another segment
    pub fn merge(&mut self, other: &LearnedWordsSegment) {
        for (domain, other_vocab) in &other.domains {
            let vocab = self.get_or_create_domain(domain);
            for entry in other_vocab.words.values() {
                vocab.add_word(entry.word.clone(), &entry.context, entry.tf_idf_score);
            }
            vocab.total_documents += other_vocab.total_documents;
        }

        // Merge global
        for (context, words) in &other.global.contexts {
            for entry in words.values() {
                self.global
                    .add_word(entry.word.clone(), context, entry.tf_idf_score);
            }
        }
        self.global.total_documents += other.global.total_documents;
    }

    /// Prune all vocabularies to configured limits
    pub fn prune_all(&mut self) {
        for vocab in self.domains.values_mut() {
            vocab.prune(self.max_words_per_domain);
        }

        for words in self.global.contexts.values_mut() {
            if words.len() > self.max_global_words_per_context {
                let mut entries: Vec<_> = words
                    .iter()
                    .map(|(k, v)| (k.clone(), v.tf_idf_score))
                    .collect();
                entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let to_keep: std::collections::HashSet<_> = entries
                    .into_iter()
                    .take(self.max_global_words_per_context)
                    .map(|(k, _)| k)
                    .collect();
                words.retain(|k, _| to_keep.contains(k));
            }
        }
    }

    /// Get statistics
    pub fn stats(&self) -> LearnedWordsStats {
        let total_domain_words: usize = self.domains.values().map(|v| v.words.len()).sum();
        let total_global_words: usize = self.global.contexts.values().map(|w| w.len()).sum();

        LearnedWordsStats {
            domain_count: self.domains.len(),
            total_domain_words,
            total_global_words,
            total_documents: self.global.total_documents,
            contexts: self.global.contexts.keys().cloned().collect(),
        }
    }

    /// List all domains
    pub fn list_domains(&self) -> Vec<&str> {
        self.domains.keys().map(|s| s.as_str()).collect()
    }

    /// Remove domain
    pub fn remove_domain(&mut self, domain: &str) -> Option<DomainVocabulary> {
        self.domains.remove(domain)
    }

    /// Clear all learned words
    pub fn clear(&mut self) {
        self.domains.clear();
        self.global = GlobalVocabulary::new();
    }

    /// Serialize segment
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Header/version
        buf.push(1); // version

        // Max limits
        write_varu32(&mut buf, self.max_words_per_domain as u32);
        write_varu32(&mut buf, self.max_global_words_per_context as u32);

        // Domains
        write_varu32(&mut buf, self.domains.len() as u32);
        for vocab in self.domains.values() {
            vocab.encode(&mut buf);
        }

        // Global
        self.global.encode(&mut buf);

        buf
    }

    /// Deserialize segment
    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError("empty learned words segment"));
        }

        let mut pos = 0usize;

        // Version
        let version = bytes[pos];
        pos += 1;
        if version != 1 {
            return Err(DecodeError("unsupported learned words version"));
        }

        // Max limits
        let max_words_per_domain = read_varu32(bytes, &mut pos)? as usize;
        let max_global_words_per_context = read_varu32(bytes, &mut pos)? as usize;

        // Domains
        let domain_count = read_varu32(bytes, &mut pos)? as usize;
        let mut domains = HashMap::with_capacity(domain_count);
        for _ in 0..domain_count {
            let vocab = DomainVocabulary::decode(bytes, &mut pos)?;
            domains.insert(vocab.domain.clone(), vocab);
        }

        // Global
        let global = GlobalVocabulary::decode(bytes, &mut pos)?;

        Ok(Self {
            domains,
            global,
            max_words_per_domain,
            max_global_words_per_context,
        })
    }
}

/// Statistics about learned words
#[derive(Debug, Clone)]
pub struct LearnedWordsStats {
    pub domain_count: usize,
    pub total_domain_words: usize,
    pub total_global_words: usize,
    pub total_documents: u32,
    pub contexts: Vec<String>,
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_learned_word_entry() {
        let entry =
            LearnedWordEntry::new("admin".to_string(), "directories".to_string()).with_score(0.85);

        assert_eq!(entry.word, "admin");
        assert_eq!(entry.tf_idf_score, 0.85);
        assert_eq!(entry.context, "directories");
    }

    #[test]
    fn test_domain_vocabulary() {
        let mut vocab = DomainVocabulary::new("example.com".to_string());

        vocab.add_word("admin".to_string(), "directories", 0.9);
        vocab.add_word("config".to_string(), "directories", 0.8);
        vocab.add_word("test".to_string(), "directories", 0.5);
        vocab.record_document();

        let top = vocab.top_words(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].word, "admin");
        assert_eq!(top[1].word, "config");
    }

    #[test]
    fn test_domain_vocabulary_prune() {
        let mut vocab = DomainVocabulary::new("example.com".to_string());

        for i in 0..100 {
            vocab.add_word(format!("word{}", i), "directories", i as f64 / 100.0);
        }

        assert_eq!(vocab.words.len(), 100);
        vocab.prune(10);
        assert_eq!(vocab.words.len(), 10);

        // Should have kept highest scoring words (90-99)
        assert!(vocab.words.contains_key("word99"));
        assert!(vocab.words.contains_key("word90"));
        assert!(!vocab.words.contains_key("word0"));
    }

    #[test]
    fn test_learned_words_segment() {
        let mut segment = LearnedWordsSegment::new();

        segment.add_word("example.com", "admin".to_string(), "directories", 0.9);
        segment.add_word("example.com", "backup".to_string(), "directories", 0.8);
        segment.add_word("other.com", "api".to_string(), "directories", 0.7);
        segment.record_document("example.com");
        segment.record_document("other.com");

        let stats = segment.stats();
        assert_eq!(stats.domain_count, 2);
        assert_eq!(stats.total_documents, 2);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut segment = LearnedWordsSegment::new();

        segment.add_word("example.com", "admin".to_string(), "directories", 0.9);
        segment.add_word("example.com", "config".to_string(), "files", 0.85);
        segment.record_document("example.com");

        let bytes = segment.serialize();
        let restored = LearnedWordsSegment::deserialize(&bytes).unwrap();

        assert_eq!(restored.domains.len(), 1);
        assert!(restored.domains.contains_key("example.com"));

        let vocab = restored.get_domain("example.com").unwrap();
        assert_eq!(vocab.words.len(), 2);
        assert!(vocab.words.contains_key("admin"));
    }

    #[test]
    fn test_global_vocabulary() {
        let mut segment = LearnedWordsSegment::new();

        segment.add_word("site1.com", "admin".to_string(), "directories", 0.9);
        segment.add_word("site2.com", "admin".to_string(), "directories", 0.8);
        segment.add_word("site1.com", "config".to_string(), "directories", 0.7);

        let global_top = segment.top_global_words("directories", 10);
        assert_eq!(global_top.len(), 2); // admin and config (admin merged from both)
    }

    #[test]
    fn test_merge_segments() {
        let mut segment1 = LearnedWordsSegment::new();
        segment1.add_word("example.com", "admin".to_string(), "directories", 0.9);

        let mut segment2 = LearnedWordsSegment::new();
        segment2.add_word("example.com", "backup".to_string(), "directories", 0.8);
        segment2.add_word("other.com", "api".to_string(), "directories", 0.7);

        segment1.merge(&segment2);

        let vocab = segment1.get_domain("example.com").unwrap();
        assert_eq!(vocab.words.len(), 2);
        assert!(segment1.domains.contains_key("other.com"));
    }
}
