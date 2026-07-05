//! ASK planner-first typed plan (ADR 0068, issue #1747).
//!
//! The planner runs *after* the deterministic AskPipeline funnel has
//! narrowed the schema slice (candidate collections + columns + scores).
//! The narrowed slice — never the raw catalog — is the only schema the
//! planner LLM ever sees. The model emits a **typed plan** whose `query`
//! step carries a read-only RQL candidate; the candidate is re-validated
//! through the production parser and the read-only classifier
//! ([`super::ask_rql_planner`]) before it can execute.
//!
//! This module is pure: it holds the plan types, the JSON plan parser, the
//! narrowed-slice prompt builder, and the routing/refusal decision. The LLM
//! transport, auto-execution, and synthesis live in the orchestrator
//! ([`super::super::impl_search`]). The [`PlannerModel`] closure seam lets
//! the routing/refusal logic be unit-tested without any HTTP round-trip.

use std::collections::BTreeSet;

use crate::api::{RedDBError, RedDBResult};

use super::ask_rql_planner::{validate_candidate, CandidateDisposition, ValidatedCandidate};

/// The intent the planner routes a question to (ADR 0068 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskIntent {
    /// "what happened to passport 123?" — generate read-only RQL over the
    /// full read-only surface, auto-execute, synthesize over the rows.
    Factual,
    /// "summarise yesterday's incidents" — retrieval-RAG cited answer.
    Synthesis,
    /// "how would I capture events into a queue?" — suggestion envelope.
    HowTo,
}

impl AskIntent {
    /// Canonical lowercase label (audit / plan summary).
    pub fn as_str(&self) -> &'static str {
        match self {
            AskIntent::Factual => "factual",
            AskIntent::Synthesis => "synthesis",
            AskIntent::HowTo => "how_to",
        }
    }

    fn parse(raw: &str) -> Option<AskIntent> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "factual" => Some(AskIntent::Factual),
            "synthesis" => Some(AskIntent::Synthesis),
            "how_to" | "howto" | "how-to" => Some(AskIntent::HowTo),
            _ => None,
        }
    }
}

/// A single funnel-narrowed candidate collection with its retrieval score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredCollection {
    pub collection: String,
    pub score: f32,
    pub columns: Vec<String>,
}

/// The funnel-narrowed schema slice handed to the planner. This is the
/// *only* schema the model sees — the raw catalog never reaches it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NarrowedSlice {
    pub collections: Vec<ScoredCollection>,
}

impl NarrowedSlice {
    /// Collection names in the slice, in narrowed order.
    pub fn collection_names(&self) -> Vec<&str> {
        self.collections
            .iter()
            .map(|c| c.collection.as_str())
            .collect()
    }
}

/// The typed plan the planner LLM emits, before candidate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskPlan {
    pub intent: AskIntent,
    /// The `query` step's read-only RQL candidate (factual intent).
    pub query: Option<String>,
    /// Model rationale / self-critique, surfaced to plan summary + audit.
    pub rationale: String,
}

impl AskPlan {
    /// A short, single-line plan summary for the audit row.
    pub fn summary(&self) -> String {
        let mut out = format!("intent={}", self.intent.as_str());
        if let Some(query) = &self.query {
            out.push_str("; query=");
            out.push_str(query.trim());
        }
        out
    }
}

/// A model that turns a planner prompt into a typed-plan JSON document.
///
/// The blanket impl over `Fn(&str) -> RedDBResult<String>` lets callers
/// pass a closure wrapping the configured planner provider (production) or
/// a canned string (tests / mock model) without a bespoke type — this is
/// the closure-model seam the routing/refusal unit tests use.
pub trait PlannerModel {
    fn plan(&self, prompt: &str) -> RedDBResult<String>;
}

impl<F> PlannerModel for F
where
    F: Fn(&str) -> RedDBResult<String>,
{
    fn plan(&self, prompt: &str) -> RedDBResult<String> {
        self(prompt)
    }
}

/// How the planner routed a plan after candidate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRouting {
    /// Factual intent with a validated **read-only** candidate ready to
    /// auto-execute under the caller's EffectiveScope.
    Execute { candidate: ValidatedCandidate },
    /// Factual intent whose candidate is **mutating** — a structured
    /// refusal. Mutating candidates are never executed under any flag; the
    /// suggestion envelope arrives in a later slice.
    RefuseMutating {
        statement_type: &'static str,
        rql: String,
    },
    /// A non-factual intent (synthesis / how-to). This slice implements the
    /// factual path only; the orchestrator decides the fallback.
    Unsupported { intent: AskIntent },
}

/// A planner pass: the prompt sent, the parsed plan, and the routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRoute {
    pub prompt: String,
    pub plan: AskPlan,
    pub routing: PlanRouting,
}

/// Assemble the planner prompt from *only* the narrowed slice. A catalog
/// with many collections never reaches the model raw: the slice is the
/// anti-drowning gate (ADR 0068 §1).
pub fn build_planner_prompt(question: &str, slice: &NarrowedSlice) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are the RedDB ASK planner. Classify the user's question intent and, for a \
         factual question, generate a single read-only RQL SELECT candidate over the \
         collections below.\n\
         Respond with ONLY a JSON object, no code fences or commentary:\n\
         {\"intent\": \"factual\"|\"synthesis\"|\"how_to\", \"query\": \"<read-only RQL or null>\", \
         \"rationale\": \"<one sentence>\"}\n\
         Rules: use only the collections and columns listed; never invent a collection; \
         a factual question MUST carry a read-only SELECT (joins and the global `any` source \
         are allowed); never emit INSERT/UPDATE/DELETE/DDL.\n\n",
    );

    if slice.collections.is_empty() {
        prompt.push_str("Candidate collections: (none)\n");
    } else {
        prompt.push_str("Candidate collections (name, score, columns):\n");
        for c in &slice.collections {
            prompt.push_str("- ");
            prompt.push_str(&c.collection);
            prompt.push_str(&format!(" (score {:.4})", c.score));
            if !c.columns.is_empty() {
                let mut cols: BTreeSet<&str> = BTreeSet::new();
                for col in &c.columns {
                    cols.insert(col.as_str());
                }
                prompt.push_str(": ");
                prompt.push_str(&cols.into_iter().collect::<Vec<_>>().join(", "));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str("\nQuestion: ");
    prompt.push_str(question);
    prompt
}

/// Parse the planner model's output into a typed plan. Tolerates code
/// fences and surrounding prose by extracting the outermost JSON object.
pub fn parse_plan(raw: &str) -> RedDBResult<AskPlan> {
    let json = extract_json_object(raw).ok_or_else(|| {
        RedDBError::Query("ASK planner returned no JSON plan object".to_string())
    })?;
    let parsed: crate::json::Value = crate::json::from_str(json)
        .map_err(|err| RedDBError::Query(format!("ASK planner returned invalid JSON plan: {err}")))?;
    let obj = parsed.as_object().ok_or_else(|| {
        RedDBError::Query("ASK planner plan must be a JSON object".to_string())
    })?;

    let intent_raw = obj
        .get("intent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RedDBError::Query("ASK planner plan is missing `intent`".to_string()))?;
    let intent = AskIntent::parse(intent_raw).ok_or_else(|| {
        RedDBError::Query(format!(
            "ASK planner returned an unknown intent `{intent_raw}`"
        ))
    })?;

    let query = obj
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let rationale = obj
        .get("rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(AskPlan {
        intent,
        query,
        rationale,
    })
}

/// Route a parsed plan: validate the factual candidate through the parser +
/// read-only classifier, refuse mutating candidates, and mark non-factual
/// intents unsupported for this slice.
pub fn route_plan(plan: &AskPlan) -> RedDBResult<PlanRouting> {
    match plan.intent {
        AskIntent::Factual => {
            let rql = plan.query.as_deref().ok_or_else(|| {
                RedDBError::Query(
                    "ASK planner classified the question as factual but produced no query \
                     candidate"
                        .to_string(),
                )
            })?;
            // Re-validate through the production parser; malformed candidates
            // are rejected here and never execute.
            let candidate = validate_candidate(rql)?;
            match candidate.disposition {
                CandidateDisposition::ReadOnly => Ok(PlanRouting::Execute { candidate }),
                CandidateDisposition::Mutating => Ok(PlanRouting::RefuseMutating {
                    statement_type: candidate.statement_type,
                    rql: candidate.rql,
                }),
            }
        }
        other => Ok(PlanRouting::Unsupported { intent: other }),
    }
}

/// Full planner pass: build the narrowed-slice prompt, call the model,
/// parse the plan, and route it. The model is the closure-model seam.
pub fn plan_and_route<M: PlannerModel>(
    question: &str,
    slice: &NarrowedSlice,
    model: &M,
) -> RedDBResult<PlannedRoute> {
    let prompt = build_planner_prompt(question, slice);
    let raw = model.plan(&prompt)?;
    let plan = parse_plan(&raw)?;
    let routing = route_plan(&plan)?;
    Ok(PlannedRoute {
        prompt,
        plan,
        routing,
    })
}

/// Extract the outermost `{...}` JSON object from a model response that may
/// carry code fences or surrounding prose.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice() -> NarrowedSlice {
        NarrowedSlice {
            collections: vec![
                ScoredCollection {
                    collection: "travelers".to_string(),
                    score: 0.91,
                    columns: vec!["passport".to_string(), "name".to_string()],
                },
                ScoredCollection {
                    collection: "trips".to_string(),
                    score: 0.44,
                    columns: vec!["passport".to_string(), "city".to_string()],
                },
            ],
        }
    }

    /// A mock planner model that returns a fixed plan JSON regardless of the
    /// prompt — stands in for the configured planner provider.
    fn mock_model(plan_json: &'static str) -> impl PlannerModel {
        move |_prompt: &str| Ok(plan_json.to_string())
    }

    #[test]
    fn prompt_contains_only_narrowed_slice() {
        let prompt = build_planner_prompt("who owns passport FDD-1?", &slice());
        assert!(prompt.contains("travelers"));
        assert!(prompt.contains("trips"));
        assert!(prompt.contains("passport"));
        // A collection outside the narrowed slice must never appear.
        assert!(!prompt.contains("secret_ledger"));
    }

    #[test]
    fn parse_plan_reads_intent_and_query() {
        let plan = parse_plan(
            "{\"intent\":\"factual\",\"query\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"lookup\"}",
        )
        .unwrap();
        assert_eq!(plan.intent, AskIntent::Factual);
        assert_eq!(
            plan.query.as_deref(),
            Some("SELECT * FROM travelers WHERE passport = 'FDD-1'")
        );
        assert_eq!(plan.rationale, "lookup");
    }

    #[test]
    fn parse_plan_tolerates_code_fences() {
        let plan = parse_plan(
            "```json\n{\"intent\":\"synthesis\",\"query\":null,\"rationale\":\"summary\"}\n```",
        )
        .unwrap();
        assert_eq!(plan.intent, AskIntent::Synthesis);
        assert!(plan.query.is_none());
    }

    #[test]
    fn parse_plan_rejects_non_json() {
        let err = parse_plan("the answer is 42").unwrap_err();
        assert!(err.to_string().contains("no JSON plan object"), "got: {err}");
    }

    #[test]
    fn parse_plan_rejects_unknown_intent() {
        let err = parse_plan("{\"intent\":\"chitchat\",\"query\":null}").unwrap_err();
        assert!(err.to_string().contains("unknown intent"), "got: {err}");
    }

    #[test]
    fn factual_plan_routes_to_executable_read_only_candidate() {
        let route = plan_and_route(
            "who owns passport FDD-1?",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"lookup\"}",
            ),
        )
        .unwrap();
        match route.routing {
            PlanRouting::Execute { candidate } => {
                assert!(candidate.is_read_only());
                assert_eq!(candidate.statement_type, "select");
            }
            other => panic!("expected Execute, got {other:?}"),
        }
    }

    #[test]
    fn factual_join_candidate_is_executable() {
        let route = plan_and_route(
            "which cities did the owner of passport FDD-1 visit?",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"SELECT * FROM travelers JOIN trips ON travelers.passport = trips.passport WHERE travelers.passport = 'FDD-1'\",\"rationale\":\"join\"}",
            ),
        )
        .unwrap();
        assert!(matches!(route.routing, PlanRouting::Execute { .. }));
    }

    #[test]
    fn factual_plan_with_mutating_candidate_is_refused_not_executed() {
        let route = plan_and_route(
            "delete traveler FDD-1",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"DELETE FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"oops\"}",
            ),
        )
        .unwrap();
        match route.routing {
            PlanRouting::RefuseMutating {
                statement_type, ..
            } => assert_eq!(statement_type, "delete"),
            other => panic!("expected RefuseMutating, got {other:?}"),
        }
    }

    #[test]
    fn factual_plan_with_malformed_candidate_is_rejected() {
        let err = plan_and_route(
            "who owns passport FDD-1?",
            &slice(),
            &mock_model("{\"intent\":\"factual\",\"query\":\"this is not rql\",\"rationale\":\"x\"}"),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("invalid RQL candidate"),
            "got: {err}"
        );
    }

    #[test]
    fn factual_plan_without_query_is_rejected() {
        let err = plan_and_route(
            "who owns passport FDD-1?",
            &slice(),
            &mock_model("{\"intent\":\"factual\",\"query\":null,\"rationale\":\"x\"}"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no query candidate"), "got: {err}");
    }

    #[test]
    fn synthesis_intent_routes_unsupported_in_this_slice() {
        let route = plan_and_route(
            "summarise yesterday's incidents",
            &slice(),
            &mock_model("{\"intent\":\"synthesis\",\"query\":null,\"rationale\":\"rag\"}"),
        )
        .unwrap();
        assert_eq!(
            route.routing,
            PlanRouting::Unsupported {
                intent: AskIntent::Synthesis
            }
        );
    }

    #[test]
    fn how_to_intent_routes_unsupported_in_this_slice() {
        let route = plan_and_route(
            "how would I capture events into a queue?",
            &slice(),
            &mock_model("{\"intent\":\"how_to\",\"query\":null,\"rationale\":\"guide\"}"),
        )
        .unwrap();
        assert_eq!(
            route.routing,
            PlanRouting::Unsupported {
                intent: AskIntent::HowTo
            }
        );
    }

    #[test]
    fn plan_summary_includes_intent_and_query() {
        let plan = AskPlan {
            intent: AskIntent::Factual,
            query: Some("SELECT * FROM travelers WHERE passport = 'FDD-1'".to_string()),
            rationale: "lookup".to_string(),
        };
        let summary = plan.summary();
        assert!(summary.contains("intent=factual"));
        assert!(summary.contains("SELECT * FROM travelers"));
    }
}
