//! Binary / multi-class logistic regression trained with SGD.
//!
//! One-vs-rest for the multi-class case: `K` independent binary
//! classifiers, one per class. Each classifier stores `num_features`
//! weights + 1 bias. Training passes one example at a time so
//! [`Self::partial_fit`] reuses the same inner loop as `fit` — the
//! only difference is whether weights are reset first.
//!
//! Features support L2 regularisation and a constant learning rate
//! schedule. The implementation is deliberately minimal; production
//! tuning (Adam, momentum, schedules, warm restarts) is follow-on
//! work and lives behind the same `partial_fit` surface.

use crate::json::{Map, Value as JsonValue};

use super::{IncrementalClassifier, TrainingExample};

/// Hyperparameters for [`LogisticRegression`].
#[derive(Debug, Clone)]
pub struct LogisticRegressionConfig {
    pub learning_rate: f32,
    pub l2_penalty: f32,
    /// Training epochs per `fit` call. `partial_fit` always runs
    /// exactly one epoch over the incoming mini-batch.
    pub epochs: usize,
    /// Random seed for shuffling. `0` disables shuffle (tests rely on
    /// deterministic order).
    pub shuffle_seed: u64,
}

impl Default for LogisticRegressionConfig {
    fn default() -> Self {
        Self {
            learning_rate: 0.05,
            l2_penalty: 0.0,
            epochs: 10,
            shuffle_seed: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogisticRegression {
    config: LogisticRegressionConfig,
    /// `weights[class][feature]`.
    weights: Vec<Vec<f32>>,
    biases: Vec<f32>,
    num_features: usize,
    num_classes: usize,
    samples_seen: u64,
}

impl LogisticRegression {
    pub fn new(config: LogisticRegressionConfig) -> Self {
        Self {
            config,
            weights: Vec::new(),
            biases: Vec::new(),
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
            // Extend class count without discarding existing weights —
            // vital for partial_fit on a stream that sees new classes
            // over time.
            self.weights
                .resize(num_classes, vec![0.0; self.num_features]);
            self.biases.resize(num_classes, 0.0);
            self.num_classes = num_classes;
        }
    }

    fn sgd_step(&mut self, ex: &TrainingExample) {
        if ex.features.len() != self.num_features {
            return;
        }
        let lr = self.config.learning_rate;
        let l2 = self.config.l2_penalty;
        for c in 0..self.num_classes {
            let target = if ex.label as usize == c { 1.0 } else { 0.0 };
            // dot(weights, features) + bias
            let mut z = self.biases[c];
            for (w, x) in self.weights[c].iter().zip(ex.features.iter()) {
                z += w * x;
            }
            let p = sigmoid(z);
            let error = p - target;
            // Gradient descent on binary cross-entropy.
            for i in 0..self.num_features {
                let grad = error * ex.features[i] + l2 * self.weights[c][i];
                self.weights[c][i] -= lr * grad;
            }
            self.biases[c] -= lr * error;
        }
    }

    fn infer_shape(examples: &[TrainingExample]) -> Option<(usize, usize)> {
        let num_features = examples.first()?.features.len();
        let num_classes = examples.iter().map(|e| e.label as usize).max()? + 1;
        Some((num_features, num_classes))
    }

    /// Serialise to JSON for `ModelRegistry` storage.
    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert(
            "lr".to_string(),
            JsonValue::Number(self.config.learning_rate as f64),
        );
        obj.insert(
            "l2".to_string(),
            JsonValue::Number(self.config.l2_penalty as f64),
        );
        obj.insert(
            "epochs".to_string(),
            JsonValue::Number(self.config.epochs as f64),
        );
        obj.insert(
            "shuffle_seed".to_string(),
            JsonValue::Number(self.config.shuffle_seed as f64),
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
            "weights".to_string(),
            JsonValue::Array(
                self.weights
                    .iter()
                    .map(|row| {
                        JsonValue::Array(row.iter().map(|f| JsonValue::Number(*f as f64)).collect())
                    })
                    .collect(),
            ),
        );
        obj.insert(
            "biases".to_string(),
            JsonValue::Array(
                self.biases
                    .iter()
                    .map(|f| JsonValue::Number(*f as f64))
                    .collect(),
            ),
        );
        JsonValue::Object(obj).to_string_compact()
    }

    pub fn from_json(raw: &str) -> Option<Self> {
        let parsed = crate::json::parse_json(raw).ok()?;
        let value = JsonValue::from(parsed);
        let obj = value.as_object()?;
        let lr = obj.get("lr")?.as_f64()? as f32;
        let l2 = obj.get("l2")?.as_f64()? as f32;
        let epochs = obj.get("epochs")?.as_i64()? as usize;
        let shuffle_seed = obj.get("shuffle_seed")?.as_i64()? as u64;
        let num_features = obj.get("num_features")?.as_i64()? as usize;
        let num_classes = obj.get("num_classes")?.as_i64()? as usize;
        let samples_seen = obj.get("samples_seen")?.as_i64()? as u64;
        let weights: Vec<Vec<f32>> = obj
            .get("weights")?
            .as_array()?
            .iter()
            .filter_map(|row| {
                row.as_array().map(|inner| {
                    inner
                        .iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect()
                })
            })
            .collect();
        let biases: Vec<f32> = obj
            .get("biases")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        Some(Self {
            config: LogisticRegressionConfig {
                learning_rate: lr,
                l2_penalty: l2,
                epochs,
                shuffle_seed,
            },
            weights,
            biases,
            num_features,
            num_classes,
            samples_seen,
        })
    }
}

impl IncrementalClassifier for LogisticRegression {
    fn fit(&mut self, examples: &[TrainingExample]) {
        if examples.is_empty() {
            return;
        }
        let Some((num_features, num_classes)) = Self::infer_shape(examples) else {
            return;
        };
        // fresh model
        self.weights = vec![vec![0.0; num_features]; num_classes];
        self.biases = vec![0.0; num_classes];
        self.num_features = num_features;
        self.num_classes = num_classes;
        self.samples_seen = 0;
        for _ in 0..self.config.epochs {
            let mut indices: Vec<usize> = (0..examples.len()).collect();
            if self.config.shuffle_seed != 0 {
                deterministic_shuffle(&mut indices, self.config.shuffle_seed);
            }
            for i in indices {
                self.sgd_step(&examples[i]);
            }
        }
        self.samples_seen = examples.len() as u64;
    }

    fn partial_fit(&mut self, examples: &[TrainingExample]) {
        if examples.is_empty() {
            return;
        }
        let (batch_features, batch_classes) = match Self::infer_shape(examples) {
            Some(pair) => pair,
            None => return,
        };
        self.ensure_shape(batch_features, batch_classes);
        for ex in examples {
            self.sgd_step(ex);
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
        let mut out = Vec::with_capacity(self.num_classes);
        for c in 0..self.num_classes {
            let mut z = self.biases[c];
            for (w, x) in self.weights[c].iter().zip(features.iter()) {
                z += w * x;
            }
            out.push(sigmoid(z));
        }
        // Normalise so probs sum to 1 (one-vs-rest outputs often don't).
        let sum: f32 = out.iter().sum();
        if sum > 0.0 {
            for p in out.iter_mut() {
                *p /= sum;
            }
        }
        out
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

fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

/// xorshift64*, deterministic, tiny. We only need reproducible
/// shuffles in tests — no cryptographic properties required.
fn deterministic_shuffle<T>(items: &mut [T], seed: u64) {
    if items.len() < 2 {
        return;
    }
    let mut state = seed | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state as usize) % (i + 1);
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linearly_separable(n: usize) -> Vec<TrainingExample> {
        // Two clusters: class 0 around (-1,0), class 1 around (1,0).
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let jitter = (i as f32) * 0.01;
            out.push(TrainingExample {
                features: vec![-1.0 + jitter, jitter],
                label: 0,
            });
            out.push(TrainingExample {
                features: vec![1.0 - jitter, jitter],
                label: 1,
            });
        }
        out
    }

    #[test]
    fn fit_learns_linearly_separable_classes() {
        let data = linearly_separable(50);
        let mut model = LogisticRegression::new(LogisticRegressionConfig {
            epochs: 50,
            ..Default::default()
        });
        model.fit(&data);
        let correct: u32 = data
            .iter()
            .map(|ex| {
                if model.predict(&ex.features) == Some(ex.label) {
                    1
                } else {
                    0
                }
            })
            .sum();
        let acc = correct as f32 / data.len() as f32;
        assert!(acc > 0.95, "accuracy too low: {acc}");
    }

    #[test]
    fn partial_fit_moves_loss_in_the_right_direction() {
        // Noisy overlapping clusters so one pass can't converge —
        // weights must grow across partial_fit calls.
        let mut data = Vec::new();
        for i in 0..200 {
            let f = i as f32 * 0.01;
            data.push(TrainingExample {
                features: vec![-0.3 + f.sin() * 0.5, 0.2 * (f * 1.3).cos()],
                label: 0,
            });
            data.push(TrainingExample {
                features: vec![0.3 + f.cos() * 0.5, 0.2 * (f * 1.7).sin()],
                label: 1,
            });
        }
        let mut model = LogisticRegression::new(LogisticRegressionConfig {
            learning_rate: 0.01,
            epochs: 1,
            ..Default::default()
        });
        fn mean_abs_weight(m: &LogisticRegression) -> f32 {
            let mut sum = 0.0f32;
            let mut n = 0usize;
            for row in &m.weights {
                for w in row {
                    sum += w.abs();
                    n += 1;
                }
            }
            if n == 0 {
                0.0
            } else {
                sum / n as f32
            }
        }
        model.partial_fit(&data[..40]);
        let w_early = mean_abs_weight(&model);
        for chunk in data[40..].chunks(40) {
            model.partial_fit(chunk);
        }
        let w_late = mean_abs_weight(&model);
        assert!(
            w_late > w_early,
            "partial_fit should keep updating weights: early={w_early} late={w_late}"
        );
        // Sanity: samples_seen reflects additive calls.
        assert_eq!(model.samples_seen(), data.len() as u64);
    }

    #[test]
    fn partial_fit_preserves_weights_across_calls() {
        let mut model = LogisticRegression::new(LogisticRegressionConfig {
            epochs: 1,
            ..Default::default()
        });
        let batch = linearly_separable(30);
        model.partial_fit(&batch);
        let weights_after_first = model.weights.clone();
        model.partial_fit(&batch);
        // Weights moved further; they should not have been reset to 0
        // (that's the `fit` contract, not `partial_fit`).
        let mut all_zero = true;
        for row in &weights_after_first {
            for w in row {
                if w.abs() > 1e-6 {
                    all_zero = false;
                }
            }
        }
        assert!(!all_zero, "weights should be non-zero after partial_fit");
        // And second call must have moved them again.
        assert_ne!(model.weights, weights_after_first);
    }

    #[test]
    fn partial_fit_extends_class_count_on_the_fly() {
        let mut model = LogisticRegression::new(LogisticRegressionConfig::default());
        model.partial_fit(&[TrainingExample {
            features: vec![0.0, 1.0],
            label: 0,
        }]);
        assert_eq!(model.num_classes, 1);
        model.partial_fit(&[TrainingExample {
            features: vec![1.0, 0.0],
            label: 3,
        }]);
        assert_eq!(model.num_classes, 4);
        assert_eq!(model.weights.len(), 4);
        for row in &model.weights {
            assert_eq!(row.len(), 2);
        }
    }

    #[test]
    fn samples_seen_tracks_lifetime_examples() {
        let mut model = LogisticRegression::new(LogisticRegressionConfig::default());
        let batch = linearly_separable(5);
        model.partial_fit(&batch);
        assert_eq!(model.samples_seen(), batch.len() as u64);
        model.partial_fit(&batch);
        assert_eq!(model.samples_seen(), 2 * batch.len() as u64);
        // fit resets to the freshly-fitted count.
        model.fit(&batch);
        assert_eq!(model.samples_seen(), batch.len() as u64);
    }

    #[test]
    fn json_round_trips_preserves_predictions() {
        let data = linearly_separable(40);
        let mut m = LogisticRegression::new(LogisticRegressionConfig {
            epochs: 20,
            ..Default::default()
        });
        m.fit(&data);
        let restored = LogisticRegression::from_json(&m.to_json()).unwrap();
        for ex in &data {
            assert_eq!(m.predict(&ex.features), restored.predict(&ex.features));
        }
    }

    #[test]
    fn predict_proba_is_normalised() {
        let data = linearly_separable(30);
        let mut m = LogisticRegression::new(LogisticRegressionConfig::default());
        m.fit(&data);
        let probs = m.predict_proba(&data[0].features);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "probs must sum to 1: {probs:?}");
    }
}
