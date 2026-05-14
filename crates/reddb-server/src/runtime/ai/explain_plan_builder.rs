//! `ExplainPlanBuilder` — pure JSON plan synthesis for `EXPLAIN ASK '...'`.
//!
//! Issue #411 (PRD #391): operators want to see the retrieval plan,
//! the source budget allocation, the provider/model the failover ladder
//! would pick, and an estimated prompt-token cost — *without* paying for
//! the LLM call. This module owns the shape of that plan output. It is
//! a deep module by the same pattern as [`super::sse_frame_encoder`],
//! [`super::audit_record_builder`], and friends:
//!
//! - No I/O, no clock, no LLM. The caller assembles inputs from the
//!   real retrieval/determinism/provider layers and hands them in.
//! - The output is a [`crate::serde_json::Value`] with a pinned key
//!   set so the wiring slice (parser → `execute_ask` → response) and
//!   downstream tests can rely on byte-stable JSON.
//! - The `EXPLAIN` path is read-only: AC says no LLM call is made
//!   and no audit row is written. Keeping this module side-effect free
//!   is what makes that guarantee enforceable by inspection.
//!
//! ## Output shape
//!
//! Top-level object, keys alphabetised (BTreeMap-backed):
//!
//! ```json
//! {
//!   "depth": 2,
//!   "determinism": { "seed": 12345, "temperature": 0.0 },
//!   "estimated_cost": {
//!       "max_completion_tokens": 1024,
//!       "prompt_tokens": 1500
//!   },
//!   "fusion": { "algorithm": "rrf", "k_constant": 60, "limit": 20 },
//!   "mode": "strict",
//!   "provider": {
//!       "model": "gpt-4o-mini",
//!       "name": "openai",
//!       "supports_citations": true,
//!       "supports_seed": true
//!   },
//!   "question": "what changed last week?",
//!   "retrieval": [
//!       { "bucket": "bm25",   "min_score": 0.0, "top_k": 20 },
//!       { "bucket": "vector", "min_score": 0.7, "top_k": 20 },
//!       { "bucket": "graph",  "min_score": 0.0, "top_k": 20 }
//!   ],
//!   "sources": [
//!       { "rank": 1, "rrf_score": 0.0327, "urn": "urn:reddb:row:42" },
//!       { "rank": 2, "rrf_score": 0.0322, "urn": "urn:reddb:row:17" }
//!   ]
//! }
//! ```
//!
//! `determinism.seed` and `determinism.temperature` are omitted when the
//! provider does not support that knob — the audit-record convention
//! from #402 (only record what the provider actually got). `sources` is
//! whatever the retrieval+fusion stages would have produced; an empty
//! list is well-formed (an honest "we'd retrieve nothing").
//!
//! ## Why a separate module
//!
//! The EXPLAIN output is part of the public surface — a debugging tool
//! that operators script against — so the shape needs to be stable
//! enough that adding a future field can't accidentally rename or shift
//! an existing one. Centralising the build, with key-set and float-
//! format tests, gives that stability cheaply.

use crate::serde_json::{Map, Value};

/// One bucket entry in the retrieval section. Mirrors the per-bucket
/// settings RRF (#398) consumes: `top_k` is the per-ranker cap, and
/// `min_score` is the per-bucket floor applied before fusion.
#[derive(Debug, Clone)]
pub struct BucketPlan {
    /// Stable bucket name. The wiring layer uses `"bm25"`, `"vector"`,
    /// `"graph"`; tests pin these.
    pub bucket: String,
    pub top_k: u32,
    pub min_score: f32,
}

/// One source row in the projected `sources` list. The EXPLAIN path
/// stops short of materialising payloads (no LLM call), so only the
/// URN and the fused RRF score are reported.
#[derive(Debug, Clone)]
pub struct PlannedSource {
    pub urn: String,
    pub rrf_score: f64,
}

/// Provider/model selection plus the relevant capability flags so a
/// reader can tell at a glance whether `STRICT` or `SEED` will take
/// effect.
#[derive(Debug, Clone)]
pub struct ProviderSelection {
    pub name: String,
    pub model: String,
    pub supports_citations: bool,
    pub supports_seed: bool,
}

/// Effective mode after `ProviderCapabilityRegistry::evaluate_mode`
/// (#396). The EXPLAIN row reports what would *actually* run, not what
/// was requested — same convention as the audit row (#402).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Strict,
    Lenient,
}

impl Mode {
    fn as_wire(self) -> &'static str {
        match self {
            Mode::Strict => "strict",
            Mode::Lenient => "lenient",
        }
    }
}

/// Determinism knobs as they will be sent to the provider. `None` means
/// the provider has no such knob (Anthropic seed, Local temperature) —
/// the key is omitted from the JSON in that case.
#[derive(Debug, Clone, Copy, Default)]
pub struct Determinism {
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
}

/// Token-budget estimates the RRF + prompt-assembly stages can produce
/// without calling the LLM. `prompt_tokens` is the assembler's best
/// guess; `max_completion_tokens` is the cap from settings (#401), not
/// a guess.
#[derive(Debug, Clone, Copy)]
pub struct EstimatedCost {
    pub prompt_tokens: u32,
    pub max_completion_tokens: u32,
}

/// All inputs the builder needs. Caller is responsible for assembling
/// these from the real retrieval/determinism/provider layers; the
/// builder does not call into them.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub question: &'a str,
    pub mode: Mode,
    pub retrieval: &'a [BucketPlan],
    pub fusion_limit: u32,
    pub fusion_k_constant: u32,
    pub depth: u32,
    pub sources: &'a [PlannedSource],
    pub provider: &'a ProviderSelection,
    pub determinism: Determinism,
    pub estimated_cost: EstimatedCost,
}

fn obj(entries: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (k, v) in entries {
        map.insert(k.to_string(), v);
    }
    Value::Object(map)
}

fn bucket_value(b: &BucketPlan) -> Value {
    obj(vec![
        ("bucket", Value::String(b.bucket.clone())),
        ("min_score", Value::Number(b.min_score as f64)),
        ("top_k", Value::Number(b.top_k as f64)),
    ])
}

fn source_value(rank: usize, s: &PlannedSource) -> Value {
    obj(vec![
        ("rank", Value::Number(rank as f64)),
        ("rrf_score", Value::Number(s.rrf_score)),
        ("urn", Value::String(s.urn.clone())),
    ])
}

fn provider_value(p: &ProviderSelection) -> Value {
    obj(vec![
        ("model", Value::String(p.model.clone())),
        ("name", Value::String(p.name.clone())),
        ("supports_citations", Value::Bool(p.supports_citations)),
        ("supports_seed", Value::Bool(p.supports_seed)),
    ])
}

fn determinism_value(d: Determinism) -> Value {
    let mut entries: Vec<(&str, Value)> = Vec::new();
    if let Some(seed) = d.seed {
        entries.push(("seed", Value::Number(seed as f64)));
    }
    if let Some(t) = d.temperature {
        entries.push(("temperature", Value::Number(t as f64)));
    }
    obj(entries)
}

fn cost_value(c: EstimatedCost) -> Value {
    obj(vec![
        (
            "max_completion_tokens",
            Value::Number(c.max_completion_tokens as f64),
        ),
        ("prompt_tokens", Value::Number(c.prompt_tokens as f64)),
    ])
}

fn fusion_value(limit: u32, k: u32) -> Value {
    obj(vec![
        ("algorithm", Value::String("rrf".to_string())),
        ("k_constant", Value::Number(k as f64)),
        ("limit", Value::Number(limit as f64)),
    ])
}

/// Build the plan JSON. Pure: same inputs → identical [`Value`] bytes.
/// The wiring layer serializes with `value.to_string_compact()` and
/// ships it as the response body.
pub fn build(inputs: &Inputs<'_>) -> Value {
    obj(vec![
        ("depth", Value::Number(inputs.depth as f64)),
        ("determinism", determinism_value(inputs.determinism)),
        ("estimated_cost", cost_value(inputs.estimated_cost)),
        (
            "fusion",
            fusion_value(inputs.fusion_limit, inputs.fusion_k_constant),
        ),
        ("mode", Value::String(inputs.mode.as_wire().to_string())),
        ("provider", provider_value(inputs.provider)),
        ("question", Value::String(inputs.question.to_string())),
        (
            "retrieval",
            Value::Array(inputs.retrieval.iter().map(bucket_value).collect()),
        ),
        (
            "sources",
            Value::Array(
                inputs
                    .sources
                    .iter()
                    .enumerate()
                    .map(|(i, s)| source_value(i + 1, s))
                    .collect(),
            ),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_openai() -> ProviderSelection {
        ProviderSelection {
            name: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            supports_citations: true,
            supports_seed: true,
        }
    }

    fn provider_anthropic() -> ProviderSelection {
        ProviderSelection {
            name: "anthropic".to_string(),
            model: "claude-opus-4-7".to_string(),
            supports_citations: true,
            supports_seed: false,
        }
    }

    fn default_buckets() -> Vec<BucketPlan> {
        vec![
            BucketPlan {
                bucket: "bm25".to_string(),
                top_k: 20,
                min_score: 0.0,
            },
            BucketPlan {
                bucket: "vector".to_string(),
                top_k: 20,
                min_score: 0.7,
            },
            BucketPlan {
                bucket: "graph".to_string(),
                top_k: 20,
                min_score: 0.0,
            },
        ]
    }

    fn fixture<'a>(
        provider: &'a ProviderSelection,
        retrieval: &'a [BucketPlan],
        sources: &'a [PlannedSource],
        determinism: Determinism,
    ) -> Inputs<'a> {
        Inputs {
            question: "what changed last week?",
            mode: Mode::Strict,
            retrieval,
            fusion_limit: 20,
            fusion_k_constant: 60,
            depth: 2,
            sources,
            provider,
            determinism,
            estimated_cost: EstimatedCost {
                prompt_tokens: 1500,
                max_completion_tokens: 1024,
            },
        }
    }

    #[test]
    fn build_emits_pinned_top_level_keys() {
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let obj = v.as_object().expect("top-level object");
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "depth",
                "determinism",
                "estimated_cost",
                "fusion",
                "mode",
                "provider",
                "question",
                "retrieval",
                "sources",
            ]
        );
    }

    #[test]
    fn build_is_deterministic_across_calls() {
        let p = provider_openai();
        let b = default_buckets();
        let s = vec![PlannedSource {
            urn: "urn:reddb:row:1".to_string(),
            rrf_score: 0.0327,
        }];
        let d = Determinism {
            temperature: Some(0.0),
            seed: Some(12345),
        };
        let a = build(&fixture(&p, &b, &s, d));
        let b2 = build(&fixture(&p, &b, &s, d));
        assert_eq!(a.to_string_compact(), b2.to_string_compact());
    }

    #[test]
    fn mode_serializes_as_lowercase_words() {
        let p = provider_openai();
        let b = default_buckets();
        let mut inp = fixture(&p, &b, &[], Determinism::default());
        inp.mode = Mode::Lenient;
        let v = build(&inp);
        assert_eq!(v.get("mode").and_then(|x| x.as_str()), Some("lenient"));
        let mut inp2 = fixture(&p, &b, &[], Determinism::default());
        inp2.mode = Mode::Strict;
        let v2 = build(&inp2);
        assert_eq!(v2.get("mode").and_then(|x| x.as_str()), Some("strict"));
    }

    #[test]
    fn determinism_omits_seed_when_provider_does_not_support_it() {
        // Anthropic-style: temperature only, no seed. The audit row
        // (#402) records only what the provider got — EXPLAIN does the
        // same, so an operator reading the plan can immediately see
        // that SEED has no effect on this provider.
        let p = provider_anthropic();
        let b = default_buckets();
        let d = Determinism {
            temperature: Some(0.0),
            seed: None,
        };
        let v = build(&fixture(&p, &b, &[], d));
        let det = v.get("determinism").and_then(|x| x.as_object()).unwrap();
        assert!(det.contains_key("temperature"));
        assert!(!det.contains_key("seed"));
    }

    #[test]
    fn determinism_omits_temperature_for_local_class_providers() {
        // Local endpoints take no temperature at all (per #396). The
        // EXPLAIN row must reflect that — a present `temperature: 0.0`
        // would be a lie.
        let p = ProviderSelection {
            name: "local".to_string(),
            model: "ggml".to_string(),
            supports_citations: false,
            supports_seed: false,
        };
        let b = default_buckets();
        let d = Determinism {
            temperature: None,
            seed: None,
        };
        let v = build(&fixture(&p, &b, &[], d));
        let det = v.get("determinism").and_then(|x| x.as_object()).unwrap();
        assert!(det.is_empty());
    }

    #[test]
    fn seed_zero_is_preserved_distinct_from_none() {
        // Same guard `DeterminismDecider` and `AnswerCacheKey` pin:
        // `Some(0)` is a real value, not a "no seed" sentinel.
        let p = provider_openai();
        let b = default_buckets();
        let d = Determinism {
            temperature: Some(0.0),
            seed: Some(0),
        };
        let v = build(&fixture(&p, &b, &[], d));
        let det = v.get("determinism").and_then(|x| x.as_object()).unwrap();
        assert!(det.contains_key("seed"));
        assert_eq!(det.get("seed").and_then(|x| x.as_u64()), Some(0));
    }

    #[test]
    fn retrieval_preserves_input_order_per_bucket() {
        // Bucket order is meaningful — RRF doesn't care, but a reader
        // scanning the plan expects bm25, vector, graph in the order
        // the wiring layer hands them in.
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let buckets = v.get("retrieval").and_then(|x| x.as_array()).unwrap();
        let names: Vec<&str> = buckets
            .iter()
            .map(|b| b.get("bucket").and_then(|x| x.as_str()).unwrap())
            .collect();
        assert_eq!(names, vec!["bm25", "vector", "graph"]);
    }

    #[test]
    fn retrieval_carries_per_bucket_min_score() {
        // BM25 0.4 and cosine 0.7 are different scales — RRF (#398)
        // applies the floor per-bucket. EXPLAIN must surface the same
        // per-bucket floor or it would mislead a reader debugging
        // `MIN_SCORE`.
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let buckets = v.get("retrieval").and_then(|x| x.as_array()).unwrap();
        let vector = &buckets[1];
        let v_score = vector.get("min_score").and_then(|x| x.as_f64()).unwrap();
        // f32 → f64 widening is lossy below the decimal — compare with
        // an epsilon rather than pinning the widened bit pattern.
        assert!((v_score - 0.7).abs() < 1e-6, "got {v_score}");
        let bm25 = &buckets[0];
        assert_eq!(bm25.get("min_score").and_then(|x| x.as_f64()), Some(0.0));
    }

    #[test]
    fn sources_emit_one_indexed_rank() {
        let p = provider_openai();
        let b = default_buckets();
        let s = vec![
            PlannedSource {
                urn: "urn:a".to_string(),
                rrf_score: 0.05,
            },
            PlannedSource {
                urn: "urn:b".to_string(),
                rrf_score: 0.04,
            },
            PlannedSource {
                urn: "urn:c".to_string(),
                rrf_score: 0.03,
            },
        ];
        let v = build(&fixture(&p, &b, &s, Determinism::default()));
        let arr = v.get("sources").and_then(|x| x.as_array()).unwrap();
        let ranks: Vec<u64> = arr
            .iter()
            .map(|s| s.get("rank").and_then(|x| x.as_u64()).unwrap())
            .collect();
        assert_eq!(ranks, vec![1, 2, 3]);
    }

    #[test]
    fn sources_preserve_input_order() {
        // The caller passes sources in their post-RRF rank order;
        // EXPLAIN must not re-sort them. Pinning here keeps the wiring
        // slice free to assume `inputs.sources[0]` is rank 1.
        let p = provider_openai();
        let b = default_buckets();
        let s = vec![
            PlannedSource {
                urn: "urn:z".to_string(),
                rrf_score: 0.05,
            },
            PlannedSource {
                urn: "urn:a".to_string(),
                rrf_score: 0.04,
            },
        ];
        let v = build(&fixture(&p, &b, &s, Determinism::default()));
        let arr = v.get("sources").and_then(|x| x.as_array()).unwrap();
        let urns: Vec<&str> = arr
            .iter()
            .map(|s| s.get("urn").and_then(|x| x.as_str()).unwrap())
            .collect();
        assert_eq!(urns, vec!["urn:z", "urn:a"]);
    }

    #[test]
    fn empty_sources_is_well_formed() {
        // No retrieval matches → empty array, not missing key.
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let arr = v.get("sources").and_then(|x| x.as_array()).unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn empty_retrieval_is_well_formed() {
        // A future single-bucket variant (e.g. text-only) might pass
        // zero buckets in some path — empty is still valid JSON.
        let p = provider_openai();
        let v = build(&fixture(&p, &[], &[], Determinism::default()));
        let arr = v.get("retrieval").and_then(|x| x.as_array()).unwrap();
        assert!(arr.is_empty());
    }

    #[test]
    fn fusion_section_pins_rrf_and_k_constant() {
        // RRF k=60 is the Cormack 2009 baseline #398 already pins;
        // EXPLAIN surfaces it so an operator can confirm what's used.
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let fusion = v.get("fusion").and_then(|x| x.as_object()).unwrap();
        assert_eq!(
            fusion.get("algorithm").and_then(|x| x.as_str()),
            Some("rrf")
        );
        assert_eq!(fusion.get("k_constant").and_then(|x| x.as_u64()), Some(60));
        assert_eq!(fusion.get("limit").and_then(|x| x.as_u64()), Some(20));
    }

    #[test]
    fn provider_section_carries_capability_flags() {
        let p = provider_anthropic();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let prov = v.get("provider").and_then(|x| x.as_object()).unwrap();
        assert_eq!(prov.get("name").and_then(|x| x.as_str()), Some("anthropic"));
        assert_eq!(
            prov.get("supports_citations").and_then(|x| x.as_bool()),
            Some(true)
        );
        assert_eq!(
            prov.get("supports_seed").and_then(|x| x.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn estimated_cost_pins_keys_and_values() {
        let p = provider_openai();
        let b = default_buckets();
        let v = build(&fixture(&p, &b, &[], Determinism::default()));
        let c = v.get("estimated_cost").and_then(|x| x.as_object()).unwrap();
        let keys: Vec<&str> = c.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["max_completion_tokens", "prompt_tokens"]);
        assert_eq!(c.get("prompt_tokens").and_then(|x| x.as_u64()), Some(1500));
        assert_eq!(
            c.get("max_completion_tokens").and_then(|x| x.as_u64()),
            Some(1024)
        );
    }

    #[test]
    fn question_is_passed_through_verbatim() {
        // No truncation, no normalisation. Operators paste questions
        // back into the next ASK call from EXPLAIN output, so byte
        // equality is the contract.
        let p = provider_openai();
        let b = default_buckets();
        let mut inp = fixture(&p, &b, &[], Determinism::default());
        let q = "weird \"quotes\" + newlines\nstill ok?";
        inp.question = q;
        let v = build(&inp);
        assert_eq!(v.get("question").and_then(|x| x.as_str()), Some(q));
    }

    #[test]
    fn depth_is_pass_through_u32() {
        let p = provider_openai();
        let b = default_buckets();
        let mut inp = fixture(&p, &b, &[], Determinism::default());
        inp.depth = 5;
        let v = build(&inp);
        assert_eq!(v.get("depth").and_then(|x| x.as_u64()), Some(5));
    }
}
