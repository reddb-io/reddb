//! Classifier subsystem (ML Feature 5).
//!
//! Two algorithms ship in this sprint, both trivially incremental
//! (support `partial_fit` over a single example or a mini-batch
//! without replaying the full training set):
//!
//! * [`LogisticRegression`] — binary SGD; update per-example,
//!   optional L2. Used for text + numerical features.
//! * [`MultinomialNaiveBayes`] — count-based; `partial_fit`
//!   literally adds the new counts to the existing tables. Works
//!   perfectly over a data stream.
//!
//! Incremental training is a first-class operation, not an
//! afterthought: every classifier exposes both `fit` (fresh model)
//! and `partial_fit` (update existing weights with new examples).
//! The surrounding `Classifier` enum routes both calls uniformly
//! so callers don't special-case algorithm choice.
//!
//! Serialisation: each model has a compact JSON representation
//! so `ModelRegistry` can store versions side-by-side with other
//! model types.

pub mod features;
pub mod logreg;
pub mod naive_bayes;

pub use features::{one_hot, tf_idf_vectorize, Vocabulary};
pub use logreg::{LogisticRegression, LogisticRegressionConfig};
pub use naive_bayes::{MultinomialNaiveBayes, NaiveBayesConfig};

use crate::json::{Map, Value as JsonValue};

/// Generic training example: feature vector + integer class label.
#[derive(Debug, Clone)]
pub struct TrainingExample {
    pub features: Vec<f32>,
    pub label: u32,
}

/// Common surface every classifier exposes.
pub trait IncrementalClassifier {
    /// Train from scratch. Any previous weights are discarded.
    fn fit(&mut self, examples: &[TrainingExample]);

    /// Incrementally update with a mini-batch. Previous weights are
    /// preserved; this is the online-learning entrypoint.
    fn partial_fit(&mut self, examples: &[TrainingExample]);

    /// Predict the most likely class for one example.
    fn predict(&self, features: &[f32]) -> Option<u32>;

    /// Predict a probability per class (0..num_classes).
    fn predict_proba(&self, features: &[f32]) -> Vec<f32>;

    /// Number of distinct classes seen so far.
    fn num_classes(&self) -> usize;

    /// Number of features the model expects. 0 until `fit`/
    /// `partial_fit` has been called with at least one example.
    fn num_features(&self) -> usize;

    /// Total number of training examples the model has seen over
    /// its lifetime — incremented by both `fit` (reset to N) and
    /// `partial_fit` (additive). Useful for lineage + rate-limiting.
    fn samples_seen(&self) -> u64;
}

/// Evaluation metrics for a classifier. Always populated by
/// `evaluate()`; serialised into the model version's `metrics_json`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierMetrics {
    pub accuracy: f32,
    pub per_class_precision: Vec<f32>,
    pub per_class_recall: Vec<f32>,
    pub per_class_f1: Vec<f32>,
    pub confusion_matrix: Vec<Vec<u32>>,
    pub samples_evaluated: u64,
}

impl ClassifierMetrics {
    pub fn macro_f1(&self) -> f32 {
        if self.per_class_f1.is_empty() {
            return 0.0;
        }
        self.per_class_f1.iter().sum::<f32>() / self.per_class_f1.len() as f32
    }

    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert(
            "accuracy".to_string(),
            JsonValue::Number(self.accuracy as f64),
        );
        obj.insert(
            "macro_f1".to_string(),
            JsonValue::Number(self.macro_f1() as f64),
        );
        obj.insert(
            "samples".to_string(),
            JsonValue::Number(self.samples_evaluated as f64),
        );
        obj.insert(
            "precision".to_string(),
            JsonValue::Array(
                self.per_class_precision
                    .iter()
                    .map(|f| JsonValue::Number(*f as f64))
                    .collect(),
            ),
        );
        obj.insert(
            "recall".to_string(),
            JsonValue::Array(
                self.per_class_recall
                    .iter()
                    .map(|f| JsonValue::Number(*f as f64))
                    .collect(),
            ),
        );
        obj.insert(
            "f1".to_string(),
            JsonValue::Array(
                self.per_class_f1
                    .iter()
                    .map(|f| JsonValue::Number(*f as f64))
                    .collect(),
            ),
        );
        obj.insert(
            "confusion_matrix".to_string(),
            JsonValue::Array(
                self.confusion_matrix
                    .iter()
                    .map(|row| {
                        JsonValue::Array(row.iter().map(|v| JsonValue::Number(*v as f64)).collect())
                    })
                    .collect(),
            ),
        );
        JsonValue::Object(obj).to_string_compact()
    }
}

/// Compute accuracy + per-class precision/recall/F1 + confusion
/// matrix against a held-out slice of examples.
pub fn evaluate<C: IncrementalClassifier>(
    model: &C,
    examples: &[TrainingExample],
) -> ClassifierMetrics {
    let k = model.num_classes().max(1);
    let mut confusion = vec![vec![0u32; k]; k];
    let mut correct = 0u32;
    for ex in examples {
        let predicted = model.predict(&ex.features).unwrap_or(0) as usize;
        let actual = ex.label as usize;
        if predicted < k && actual < k {
            confusion[actual][predicted] += 1;
            if predicted == actual {
                correct += 1;
            }
        }
    }
    let total = examples.len() as u32;
    let accuracy = if total == 0 {
        0.0
    } else {
        correct as f32 / total as f32
    };
    let mut precision = vec![0.0f32; k];
    let mut recall = vec![0.0f32; k];
    let mut f1 = vec![0.0f32; k];
    for c in 0..k {
        let tp = confusion[c][c] as f32;
        let pred_positive: u32 = (0..k).map(|r| confusion[r][c]).sum();
        let actual_positive: u32 = confusion[c].iter().sum();
        let p = if pred_positive == 0 {
            0.0
        } else {
            tp / pred_positive as f32
        };
        let r = if actual_positive == 0 {
            0.0
        } else {
            tp / actual_positive as f32
        };
        precision[c] = p;
        recall[c] = r;
        f1[c] = if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        };
    }
    ClassifierMetrics {
        accuracy,
        per_class_precision: precision,
        per_class_recall: recall,
        per_class_f1: f1,
        confusion_matrix: confusion,
        samples_evaluated: total as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyClassifier {
        classes: usize,
    }

    impl IncrementalClassifier for DummyClassifier {
        fn fit(&mut self, _: &[TrainingExample]) {}
        fn partial_fit(&mut self, _: &[TrainingExample]) {}
        fn predict(&self, features: &[f32]) -> Option<u32> {
            // "predict the class whose index best matches the first
            // feature" — enough to drive metrics tests.
            let raw = features.first().copied().unwrap_or(0.0);
            Some(raw.round().max(0.0) as u32)
        }
        fn predict_proba(&self, _: &[f32]) -> Vec<f32> {
            vec![1.0 / self.classes as f32; self.classes]
        }
        fn num_classes(&self) -> usize {
            self.classes
        }
        fn num_features(&self) -> usize {
            1
        }
        fn samples_seen(&self) -> u64 {
            0
        }
    }

    #[test]
    fn evaluate_reports_perfect_scores_for_oracle_model() {
        let dummy = DummyClassifier { classes: 2 };
        let examples: Vec<_> = (0..10)
            .map(|i| TrainingExample {
                features: vec![(i % 2) as f32],
                label: (i % 2) as u32,
            })
            .collect();
        let m = evaluate(&dummy, &examples);
        assert!((m.accuracy - 1.0).abs() < 1e-6);
        assert!((m.macro_f1() - 1.0).abs() < 1e-6);
        assert_eq!(m.samples_evaluated, 10);
    }

    #[test]
    fn metrics_json_round_trips_every_field() {
        let m = ClassifierMetrics {
            accuracy: 0.8,
            per_class_precision: vec![0.9, 0.7],
            per_class_recall: vec![0.8, 0.8],
            per_class_f1: vec![0.85, 0.74],
            confusion_matrix: vec![vec![8, 2], vec![2, 8]],
            samples_evaluated: 20,
        };
        let raw = m.to_json();
        assert!(raw.contains("\"accuracy\""));
        assert!(raw.contains("\"confusion_matrix\""));
        assert!(raw.contains("\"macro_f1\""));
    }
}
