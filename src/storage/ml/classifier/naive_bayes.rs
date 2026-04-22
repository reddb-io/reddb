//! Multinomial Naive Bayes with Laplace smoothing.
//!
//! Ideal counterpart to [`super::LogisticRegression`] for the
//! incremental story: `partial_fit` is a pure additive update
//! (class counts + per-feature counts accumulate), so you get
//! identical results whether you train on the whole set in one
//! pass or drip-feed examples through many calls. No epochs, no
//! learning rate, no randomisation — the algorithm is
//! deterministic given its counts.
//!
//! Features are treated as non-negative counts (TF-IDF values
//! also work as long as they're ≥ 0). Smoothing parameter is
//! configurable; default `alpha = 1.0` (classic Laplace).

use crate::json::{Map, Value as JsonValue};

use super::{IncrementalClassifier, TrainingExample};

#[derive(Debug, Clone)]
pub struct NaiveBayesConfig {
    pub alpha: f32,
}

impl Default for NaiveBayesConfig {
    fn default() -> Self {
        Self { alpha: 1.0 }
    }
}

#[derive(Debug, Clone)]
pub struct MultinomialNaiveBayes {
    config: NaiveBayesConfig,
    /// `class_counts[c]` = number of examples seen with label c.
    class_counts: Vec<u64>,
    /// `feature_counts[c][f]` = sum of `features[f]` across examples
    /// with label c.
    feature_counts: Vec<Vec<f64>>,
    /// `feature_totals[c]` = total mass in class c (for smoothing).
    feature_totals: Vec<f64>,
    num_features: usize,
    num_classes: usize,
    samples_seen: u64,
}

impl MultinomialNaiveBayes {
    pub fn new(config: NaiveBayesConfig) -> Self {
        Self {
            config,
            class_counts: Vec::new(),
            feature_counts: Vec::new(),
            feature_totals: Vec::new(),
            num_features: 0,
            num_classes: 0,
            samples_seen: 0,
        }
    }

    fn ensure_shape(&mut self, num_features: usize, num_classes: usize) {
        if self.num_features == 0 {
            self.num_features = num_features;
        }
        if num_classes > self.num_classes {
            self.class_counts.resize(num_classes, 0);
            self.feature_counts
                .resize(num_classes, vec![0.0; self.num_features]);
            self.feature_totals.resize(num_classes, 0.0);
            self.num_classes = num_classes;
        }
    }

    fn accumulate(&mut self, ex: &TrainingExample) {
        if ex.features.len() != self.num_features {
            return;
        }
        let c = ex.label as usize;
        self.class_counts[c] += 1;
        let mut total = 0.0;
        for (i, &v) in ex.features.iter().enumerate() {
            if v < 0.0 {
                continue; // counts stay non-negative
            }
            self.feature_counts[c][i] += v as f64;
            total += v as f64;
        }
        self.feature_totals[c] += total;
    }

    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert(
            "alpha".to_string(),
            JsonValue::Number(self.config.alpha as f64),
        );
        obj.insert(
            "num_features".to_string(),
            JsonValue::Number(self.num_features as f64),
        );
        obj.insert(
            "num_classes".to_string(),
            JsonValue::Number(self.num_classes as f64),
        );
        obj.insert(
            "samples_seen".to_string(),
            JsonValue::Number(self.samples_seen as f64),
        );
        obj.insert(
            "class_counts".to_string(),
            JsonValue::Array(
                self.class_counts
                    .iter()
                    .map(|v| JsonValue::Number(*v as f64))
                    .collect(),
            ),
        );
        obj.insert(
            "feature_counts".to_string(),
            JsonValue::Array(
                self.feature_counts
                    .iter()
                    .map(|row| {
                        JsonValue::Array(row.iter().map(|v| JsonValue::Number(*v)).collect())
                    })
                    .collect(),
            ),
        );
        obj.insert(
            "feature_totals".to_string(),
            JsonValue::Array(
                self.feature_totals
                    .iter()
                    .map(|v| JsonValue::Number(*v))
                    .collect(),
            ),
        );
        JsonValue::Object(obj).to_string_compact()
    }

    pub fn from_json(raw: &str) -> Option<Self> {
        let parsed = crate::json::parse_json(raw).ok()?;
        let value = JsonValue::from(parsed);
        let obj = value.as_object()?;
        let alpha = obj.get("alpha")?.as_f64()? as f32;
        let num_features = obj.get("num_features")?.as_i64()? as usize;
        let num_classes = obj.get("num_classes")?.as_i64()? as usize;
        let samples_seen = obj.get("samples_seen")?.as_i64()? as u64;
        let class_counts: Vec<u64> = obj
            .get("class_counts")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_i64().map(|i| i as u64))
            .collect();
        let feature_counts: Vec<Vec<f64>> = obj
            .get("feature_counts")?
            .as_array()?
            .iter()
            .filter_map(|row| {
                row.as_array().map(|inner| {
                    inner
                        .iter()
                        .filter_map(|v| v.as_f64())
                        .collect::<Vec<f64>>()
                })
            })
            .collect();
        let feature_totals: Vec<f64> = obj
            .get("feature_totals")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_f64())
            .collect();
        Some(Self {
            config: NaiveBayesConfig { alpha },
            class_counts,
            feature_counts,
            feature_totals,
            num_features,
            num_classes,
            samples_seen,
        })
    }
}

impl IncrementalClassifier for MultinomialNaiveBayes {
    fn fit(&mut self, examples: &[TrainingExample]) {
        if examples.is_empty() {
            return;
        }
        let num_features = examples[0].features.len();
        let num_classes = examples.iter().map(|e| e.label as usize).max().unwrap() + 1;
        self.class_counts = vec![0; num_classes];
        self.feature_counts = vec![vec![0.0; num_features]; num_classes];
        self.feature_totals = vec![0.0; num_classes];
        self.num_features = num_features;
        self.num_classes = num_classes;
        self.samples_seen = 0;
        for ex in examples {
            self.accumulate(ex);
        }
        self.samples_seen = examples.len() as u64;
    }

    fn partial_fit(&mut self, examples: &[TrainingExample]) {
        if examples.is_empty() {
            return;
        }
        let num_features = examples[0].features.len();
        let num_classes = examples.iter().map(|e| e.label as usize).max().unwrap() + 1;
        self.ensure_shape(num_features, num_classes);
        for ex in examples {
            self.accumulate(ex);
        }
        self.samples_seen = self.samples_seen.saturating_add(examples.len() as u64);
    }

    fn predict(&self, features: &[f32]) -> Option<u32> {
        let probs = self.predict_proba(features);
        if probs.is_empty() {
            return None;
        }
        let mut best = 0usize;
        let mut best_p = probs[0];
        for (i, &p) in probs.iter().enumerate().skip(1) {
            if p > best_p {
                best_p = p;
                best = i;
            }
        }
        Some(best as u32)
    }

    fn predict_proba(&self, features: &[f32]) -> Vec<f32> {
        if features.len() != self.num_features || self.num_classes == 0 {
            return Vec::new();
        }
        let total_samples: u64 = self.class_counts.iter().sum();
        if total_samples == 0 {
            return vec![1.0 / self.num_classes as f32; self.num_classes];
        }
        let alpha = self.config.alpha as f64;
        let mut log_scores = vec![0f64; self.num_classes];
        for c in 0..self.num_classes {
            let prior = (self.class_counts[c] as f64).max(f64::MIN_POSITIVE) / total_samples as f64;
            let mut lp = prior.ln();
            let denom = self.feature_totals[c] + alpha * self.num_features as f64;
            for (i, &x) in features.iter().enumerate() {
                if x <= 0.0 {
                    continue;
                }
                let numer = self.feature_counts[c][i] + alpha;
                lp += (x as f64) * (numer / denom).ln();
            }
            log_scores[c] = lp;
        }
        // Softmax over log-scores → normalised probabilities.
        let max = log_scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mut probs = Vec::with_capacity(self.num_classes);
        let mut sum = 0.0f64;
        for lp in &log_scores {
            let v = (lp - max).exp();
            probs.push(v);
            sum += v;
        }
        if sum > 0.0 {
            for p in probs.iter_mut() {
                *p /= sum;
            }
        }
        probs.into_iter().map(|p| p as f32).collect()
    }

    fn num_classes(&self) -> usize {
        self.num_classes
    }

    fn num_features(&self) -> usize {
        self.num_features
    }

    fn samples_seen(&self) -> u64 {
        self.samples_seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-class bag-of-words dataset: class 0 docs mention "cat",
    /// class 1 docs mention "dog". Three-dim vectors: [cat, dog, the].
    fn bow_dataset() -> Vec<TrainingExample> {
        vec![
            TrainingExample {
                features: vec![3.0, 0.0, 1.0],
                label: 0,
            },
            TrainingExample {
                features: vec![2.0, 0.0, 2.0],
                label: 0,
            },
            TrainingExample {
                features: vec![4.0, 0.0, 0.0],
                label: 0,
            },
            TrainingExample {
                features: vec![0.0, 3.0, 1.0],
                label: 1,
            },
            TrainingExample {
                features: vec![0.0, 4.0, 2.0],
                label: 1,
            },
            TrainingExample {
                features: vec![0.0, 2.0, 1.0],
                label: 1,
            },
        ]
    }

    #[test]
    fn fit_learns_bow_dataset() {
        let data = bow_dataset();
        let mut m = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        m.fit(&data);
        for ex in &data {
            assert_eq!(m.predict(&ex.features), Some(ex.label));
        }
    }

    #[test]
    fn partial_fit_equivalent_to_fit_on_full_set() {
        let data = bow_dataset();
        let mut full = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        full.fit(&data);
        let mut incremental = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        for ex in &data {
            incremental.partial_fit(std::slice::from_ref(ex));
        }
        // Predictions on the training set must agree — NB with the
        // same counts produces identical probabilities.
        for ex in &data {
            assert_eq!(
                full.predict(&ex.features),
                incremental.predict(&ex.features)
            );
        }
        assert_eq!(full.class_counts, incremental.class_counts);
        assert_eq!(full.feature_counts, incremental.feature_counts);
        assert_eq!(full.feature_totals, incremental.feature_totals);
    }

    #[test]
    fn partial_fit_is_associative() {
        let data = bow_dataset();
        let mut one_shot = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        one_shot.partial_fit(&data);
        let mut split = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        split.partial_fit(&data[..3]);
        split.partial_fit(&data[3..]);
        assert_eq!(one_shot.class_counts, split.class_counts);
        assert_eq!(one_shot.feature_counts, split.feature_counts);
    }

    #[test]
    fn partial_fit_extends_class_count() {
        let mut m = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        m.partial_fit(&[TrainingExample {
            features: vec![1.0, 0.0],
            label: 0,
        }]);
        m.partial_fit(&[TrainingExample {
            features: vec![0.0, 1.0],
            label: 2,
        }]);
        assert_eq!(m.num_classes(), 3);
        // Class 1 was never seen — counts stay zero.
        assert_eq!(m.class_counts[1], 0);
    }

    #[test]
    fn predict_proba_sums_to_one_and_has_correct_length() {
        let data = bow_dataset();
        let mut m = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        m.fit(&data);
        let p = m.predict_proba(&vec![1.0, 0.0, 1.0]);
        assert_eq!(p.len(), 2);
        let sum: f32 = p.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "{p:?}");
        assert!(p[0] > p[1], "cat-heavy doc should prefer class 0: {p:?}");
    }

    #[test]
    fn json_round_trips() {
        let data = bow_dataset();
        let mut m = MultinomialNaiveBayes::new(NaiveBayesConfig::default());
        m.fit(&data);
        let back = MultinomialNaiveBayes::from_json(&m.to_json()).unwrap();
        for ex in &data {
            assert_eq!(m.predict(&ex.features), back.predict(&ex.features));
        }
    }
}
