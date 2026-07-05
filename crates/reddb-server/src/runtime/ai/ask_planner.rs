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

    /// The funnel grounded nothing: no candidate collection survived the
    /// narrowing. An empty slice must never reach the planner LLM — grounding
    /// failure is answered honestly, not by inventing a query over `(none)`.
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty()
    }
}

/// A raw suggested statement the planner emitted for a how-to question,
/// before parser validation. `rql` is the candidate statement text exactly as
/// the model produced it; it is never trusted until it passes the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSuggestion {
    pub rql: String,
    pub rationale: String,
}

/// A parser-validated suggested statement in the how-to envelope. It carries
/// the `mutating` flag and canonical statement kind and is **advisory only**:
/// suggested statements — including mutating/DDL ones — are NEVER executed by
/// ASK. A future apply-command consumes this envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedStatement {
    /// The candidate RQL, trimmed, exactly as it parsed.
    pub rql: String,
    /// True when the statement writes / drops / alters / otherwise mutates
    /// state (or is any non-read-only kind). Advisory — never a licence to run.
    pub mutating: bool,
    /// Canonical statement-type label (`select`, `insert`, `create_queue`, …).
    pub statement_type: &'static str,
    /// Why the planner suggested this statement.
    pub rationale: String,
}

/// The typed plan the planner LLM emits, before candidate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskPlan {
    pub intent: AskIntent,
    /// The `query` step's read-only RQL candidate (factual intent).
    pub query: Option<String>,
    /// The natural-language answer explaining the approach (how-to intent).
    /// Empty for the factual/synthesis paths, which synthesize their answer.
    pub answer: String,
    /// The how-to suggestion: raw statements before parser validation. Each is
    /// re-validated through the production parser by [`route_plan`].
    pub suggestion: Vec<RawSuggestion>,
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

/// Validate each suggested statement through the production parser, **dropping**
/// any that do not parse (unparseable model output is never returned raw), and
/// flag each read-only vs mutating. Mutating/DDL statements are kept in the
/// envelope — so a future apply-command can consume them — but ASK never runs
/// them; this function only classifies, it never executes.
pub fn validate_suggestions(raw: &[RawSuggestion]) -> Vec<SuggestedStatement> {
    let mut out = Vec::new();
    for item in raw {
        // A candidate that fails the production parser is dropped, never
        // returned raw. The suggestion is advisory regardless of disposition.
        if let Ok(candidate) = validate_candidate(&item.rql) {
            out.push(SuggestedStatement {
                mutating: !candidate.is_read_only(),
                statement_type: candidate.statement_type,
                rql: candidate.rql,
                rationale: item.rationale.clone(),
            });
        }
    }
    out
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
    /// How-to intent: an advisory suggestion envelope. `answer` explains the
    /// approach in natural language; `suggestion` carries the parser-validated
    /// statements, each flagged `mutating`, plus their rationale. Suggested
    /// statements — including mutating/DDL ones — are NEVER executed; a future
    /// apply-command consumes this envelope.
    Suggest {
        answer: String,
        suggestion: Vec<SuggestedStatement>,
    },
    /// A remaining non-factual intent (synthesis). The orchestrator decides
    /// the fallback (retrieval-RAG cited answer).
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
    let json = extract_json_object(raw)
        .ok_or_else(|| RedDBError::Query("ASK planner returned no JSON plan object".to_string()))?;
    let parsed: crate::json::Value = crate::json::from_str(json).map_err(|err| {
        RedDBError::Query(format!("ASK planner returned invalid JSON plan: {err}"))
    })?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| RedDBError::Query("ASK planner plan must be a JSON object".to_string()))?;

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

    let answer = obj
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let suggestion = obj
        .get("suggestion")
        .and_then(|v| v.as_array())
        .map(parse_suggestions)
        .unwrap_or_default();

    let rationale = obj
        .get("rationale")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    Ok(AskPlan {
        intent,
        query,
        answer,
        suggestion,
        rationale,
    })
}

/// Read the `suggestion` array of a how-to plan into raw statements. Each item
/// may be a bare RQL string or a `{ "rql": "...", "rationale": "..." }` object;
/// items without any statement text are skipped. Parser validation happens
/// later in [`validate_suggestions`], not here.
fn parse_suggestions(items: &[crate::json::Value]) -> Vec<RawSuggestion> {
    let mut out = Vec::new();
    for item in items {
        if let Some(rql) = item.as_str() {
            let rql = rql.trim();
            if !rql.is_empty() {
                out.push(RawSuggestion {
                    rql: rql.to_string(),
                    rationale: String::new(),
                });
            }
        } else if let Some(obj) = item.as_object() {
            let rql = obj
                .get("rql")
                .or_else(|| obj.get("statement"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if rql.is_empty() {
                continue;
            }
            let rationale = obj
                .get("rationale")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            out.push(RawSuggestion { rql, rationale });
        }
    }
    out
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
        AskIntent::HowTo => {
            // Parser-validate every suggested statement; unparseable ones are
            // dropped. Mutating/DDL statements survive in the envelope but are
            // never executed — the suggestion is advisory.
            let suggestion = validate_suggestions(&plan.suggestion);
            Ok(PlanRouting::Suggest {
                answer: plan.answer.clone(),
                suggestion,
            })
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

// ===========================================================================
// Plan budget (ADR 0068 §4, issue #1748)
// ===========================================================================

/// Default `red.config.ai.ask.max_plan_steps` cap when unset.
pub const DEFAULT_MAX_PLAN_STEPS: usize = 3;

/// A budgeted step in the executed plan. `refine_retrieval` and `query` are
/// the steps the budget accounts for; each consumes exactly one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStep {
    /// A single re-funnel with expanded tokens after grounding fails on the
    /// first pass (ADR 0013 single-retry analogy).
    RefineRetrieval,
    /// The read-only RQL candidate the planner emitted.
    Query,
}

impl PlanStep {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanStep::RefineRetrieval => "refine_retrieval",
            PlanStep::Query => "query",
        }
    }
}

/// Resolve the effective plan-step budget: a per-query `STEPS N` request is
/// clamped to the configured `max_plan_steps` cap and never exceeds it; an
/// absent request falls back to the cap. The result is always at least 1.
pub fn clamp_plan_steps(requested: Option<usize>, cap: usize) -> usize {
    let cap = cap.max(1);
    match requested {
        Some(n) => n.clamp(1, cap),
        None => cap,
    }
}

/// The budget was exhausted mid-plan: a step was attempted after the cap was
/// already reached. The orchestrator turns this into a structured
/// partial-with-warning result rather than looping unbounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExhausted {
    pub attempted: PlanStep,
    pub executed_steps: usize,
    pub max_steps: usize,
}

/// A bounded plan-step counter. Total executed steps can never exceed
/// `max_steps` — the anti-unbounded-loop guard (ADR 0068 §4).
#[derive(Debug, Clone)]
pub struct PlanBudget {
    max_steps: usize,
    executed: Vec<PlanStep>,
}

impl PlanBudget {
    /// Build a budget from a per-query `STEPS N` request and the config cap.
    /// The request is clamped so it can never exceed the cap.
    pub fn new(requested_steps: Option<usize>, cap: usize) -> Self {
        Self {
            max_steps: clamp_plan_steps(requested_steps, cap),
            executed: Vec::new(),
        }
    }

    /// The clamped ceiling on total executed steps.
    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    /// How many steps have executed so far.
    pub fn executed_count(&self) -> usize {
        self.executed.len()
    }

    /// Steps still available before the budget is exhausted.
    pub fn remaining(&self) -> usize {
        self.max_steps.saturating_sub(self.executed.len())
    }

    /// The executed steps, in order — surfaced to the audit row.
    pub fn executed_steps(&self) -> &[PlanStep] {
        &self.executed
    }

    /// Charge one plan step. Returns `Err(BudgetExhausted)` — without
    /// recording the step — when the cap is already reached.
    pub fn charge(&mut self, step: PlanStep) -> Result<(), BudgetExhausted> {
        if self.executed.len() >= self.max_steps {
            return Err(BudgetExhausted {
                attempted: step,
                executed_steps: self.executed.len(),
                max_steps: self.max_steps,
            });
        }
        self.executed.push(step);
        Ok(())
    }
}

// ===========================================================================
// Grounding critique + single refine_retrieval retry (ADR 0068 §4)
// ===========================================================================

/// The funnel behind a closure seam so the retry/refusal logic is unit-tested
/// without a live retrieval pass. `expanded` requests the refine_retrieval
/// widening (expanded tokens, relaxed score floor) on the single retry.
pub trait RetrievalFunnel {
    fn funnel(&self, expanded: bool) -> RedDBResult<NarrowedSlice>;
}

impl<F> RetrievalFunnel for F
where
    F: Fn(bool) -> RedDBResult<NarrowedSlice>,
{
    fn funnel(&self, expanded: bool) -> RedDBResult<NarrowedSlice> {
        self(expanded)
    }
}

/// The outcome of the grounding critique: either a grounded slice (possibly
/// after the single refine_retrieval retry) or an honest "no matching
/// sources" — never an invented answer.
#[derive(Debug, Clone, PartialEq)]
pub enum GroundingOutcome {
    /// A slice that grounds. `refined` is true when the single
    /// refine_retrieval retry produced it.
    Grounded { slice: NarrowedSlice, refined: bool },
    /// Both the first funnel pass and the single refine_retrieval retry
    /// grounded nothing. ASK answers honestly instead of inventing.
    NoMatchingSources,
}

/// Fold the self-critique into grounding: run the funnel; if it grounds
/// nothing, re-funnel **exactly once** with expanded tokens (mirroring the
/// single citation retry of ADR 0013); if that still grounds nothing, report
/// `NoMatchingSources` so ASK answers honestly rather than inventing.
pub fn ground_with_refine<F: RetrievalFunnel>(funnel: &F) -> RedDBResult<GroundingOutcome> {
    let first = funnel.funnel(false)?;
    if !first.is_empty() {
        return Ok(GroundingOutcome::Grounded {
            slice: first,
            refined: false,
        });
    }
    // Exactly one refine_retrieval re-funnel with expanded tokens.
    let refined = funnel.funnel(true)?;
    if !refined.is_empty() {
        return Ok(GroundingOutcome::Grounded {
            slice: refined,
            refined: true,
        });
    }
    Ok(GroundingOutcome::NoMatchingSources)
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
        assert!(
            err.to_string().contains("no JSON plan object"),
            "got: {err}"
        );
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
            PlanRouting::RefuseMutating { statement_type, .. } => {
                assert_eq!(statement_type, "delete")
            }
            other => panic!("expected RefuseMutating, got {other:?}"),
        }
    }

    #[test]
    fn factual_plan_with_malformed_candidate_is_rejected() {
        let err = plan_and_route(
            "who owns passport FDD-1?",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"this is not rql\",\"rationale\":\"x\"}",
            ),
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
    fn synthesis_intent_carries_the_routed_intent_for_the_rag_fallthrough() {
        // #1749: a summarise/explain question routes to the RAG path. The
        // routing carries the intent so the orchestrator can record it on the
        // downstream audit row without re-classifying.
        let route = plan_and_route(
            "summarise the incidents from yesterday",
            &slice(),
            &mock_model("{\"intent\":\"synthesis\",\"query\":null,\"rationale\":\"rag\"}"),
        )
        .unwrap();
        match route.routing {
            PlanRouting::Unsupported { intent } => {
                assert_eq!(intent, AskIntent::Synthesis);
                assert_eq!(intent.as_str(), "synthesis");
            }
            other => panic!("expected Unsupported{{synthesis}}, got {other:?}"),
        }
    }

    #[test]
    fn calculation_question_routes_factual_not_synthesis() {
        // ADR 0013 conformance boundary (#1749): a calculation-shaped question
        // must land on the *factual* intent — a read-only aggregate the engine
        // computes — never synthesis, where the LLM would be free to invent the
        // number. The planner emits `factual` + a read-only aggregate SELECT;
        // routing executes it and never falls through to RAG synthesis.
        let route = plan_and_route(
            "how many trips went to Lisbon?",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"SELECT COUNT(*) FROM trips WHERE city = 'Lisbon'\",\"rationale\":\"aggregate count\"}",
            ),
        )
        .unwrap();
        assert_ne!(
            route.plan.intent,
            AskIntent::Synthesis,
            "a calculation must never classify as synthesis (the LLM must not invent numbers)"
        );
        match route.routing {
            PlanRouting::Execute { candidate } => {
                assert!(candidate.is_read_only());
                assert_eq!(candidate.statement_type, "select");
            }
            other => {
                panic!("a calculation must route to an executable factual candidate, got {other:?}")
            }
        }
    }

    #[test]
    fn how_to_intent_routes_to_a_validated_suggestion_envelope() {
        // A how-to plan carries a natural-language answer plus a suggestion of
        // parser-validated statements — a read-only SELECT, a mutating DDL
        // CREATE QUEUE, and a mutating EVENTS BACKFILL.
        let route = plan_and_route(
            "how would I capture events from orders into a queue?",
            &slice(),
            &mock_model(
                "{\"intent\":\"how_to\",\"answer\":\"Create a queue and backfill events into it.\",\
                  \"suggestion\":[\
                    {\"rql\":\"CREATE QUEUE events_q WORK\",\"rationale\":\"the sink queue\"},\
                    {\"rql\":\"EVENTS BACKFILL orders TO events_q\",\"rationale\":\"seed history\"},\
                    {\"rql\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"inspect\"}\
                  ],\"rationale\":\"guide\"}",
            ),
        )
        .unwrap();
        match route.routing {
            PlanRouting::Suggest { answer, suggestion } => {
                assert!(answer.contains("Create a queue"));
                assert_eq!(suggestion.len(), 3);
                // DDL / mutating statements are present and flagged mutating,
                // and are NEVER executed — this envelope is advisory only.
                assert!(suggestion[0].mutating, "CREATE QUEUE is mutating DDL");
                assert!(suggestion[1].mutating, "EVENTS BACKFILL is mutating");
                assert!(!suggestion[2].mutating, "the SELECT is read-only");
                assert_eq!(suggestion[2].statement_type, "select");
                assert_eq!(suggestion[0].rationale, "the sink queue");
            }
            other => panic!("expected Suggest, got {other:?}"),
        }
    }

    #[test]
    fn how_to_suggestion_drops_unparseable_statements_never_returns_them_raw() {
        // The middle statement is not valid RQL; it is dropped, and only the
        // two parser-valid statements survive in the envelope.
        let route = plan_and_route(
            "how do I list travelers?",
            &slice(),
            &mock_model(
                "{\"intent\":\"how_to\",\"answer\":\"Select from the collection.\",\
                  \"suggestion\":[\
                    {\"rql\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\"},\
                    {\"rql\":\"this is not rql at all\"},\
                    \"DELETE FROM travelers WHERE passport = 'FDD-1'\"\
                  ]}",
            ),
        )
        .unwrap();
        match route.routing {
            PlanRouting::Suggest { suggestion, .. } => {
                assert_eq!(suggestion.len(), 2, "the unparseable statement is dropped");
                assert_eq!(suggestion[0].statement_type, "select");
                // A bare-string suggestion item is accepted and validated too.
                assert_eq!(suggestion[1].statement_type, "delete");
                assert!(suggestion[1].mutating);
                for s in &suggestion {
                    assert!(
                        !s.rql.contains("not rql"),
                        "raw unparseable text must never survive"
                    );
                }
            }
            other => panic!("expected Suggest, got {other:?}"),
        }
    }

    #[test]
    fn routing_distinguishes_how_to_from_factual_at_the_closure_model_seam() {
        // Same slice + question, only the model's intent classification differs
        // — the closure-model seam is what routes factual vs how-to.
        let factual = plan_and_route(
            "how would I capture events into a queue?",
            &slice(),
            &mock_model(
                "{\"intent\":\"factual\",\"query\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"x\"}",
            ),
        )
        .unwrap();
        assert!(matches!(factual.routing, PlanRouting::Execute { .. }));

        let how_to = plan_and_route(
            "how would I capture events into a queue?",
            &slice(),
            &mock_model(
                "{\"intent\":\"how_to\",\"answer\":\"Use a queue.\",\"suggestion\":[\"CREATE QUEUE events_q WORK\"],\"rationale\":\"x\"}",
            ),
        )
        .unwrap();
        assert!(matches!(how_to.routing, PlanRouting::Suggest { .. }));
    }

    // -----------------------------------------------------------------------
    // Plan budget: STEPS clamp + bounded step counter (#1748).
    // -----------------------------------------------------------------------

    #[test]
    fn steps_clause_is_clamped_to_the_config_cap_never_exceeding_it() {
        // Above the cap → clamped down to the cap.
        assert_eq!(clamp_plan_steps(Some(9), 3), 3);
        // At/below the cap → honored verbatim.
        assert_eq!(clamp_plan_steps(Some(2), 3), 2);
        assert_eq!(clamp_plan_steps(Some(3), 3), 3);
        // Absent → falls back to the cap.
        assert_eq!(clamp_plan_steps(None, 3), 3);
        // Never below 1, even for a zero cap or a zero request.
        assert_eq!(clamp_plan_steps(Some(0), 3), 1);
        assert_eq!(clamp_plan_steps(None, 0), 1);
    }

    #[test]
    fn plan_budget_charges_until_exhausted_then_refuses_more() {
        let mut budget = PlanBudget::new(Some(2), 3);
        assert_eq!(budget.max_steps(), 2);
        assert_eq!(budget.remaining(), 2);

        assert!(budget.charge(PlanStep::RefineRetrieval).is_ok());
        assert_eq!(budget.remaining(), 1);
        assert!(budget.charge(PlanStep::Query).is_ok());
        assert_eq!(budget.remaining(), 0);
        assert_eq!(
            budget.executed_steps(),
            &[PlanStep::RefineRetrieval, PlanStep::Query]
        );

        // The third step exhausts the budget — no unbounded loop.
        let err = budget.charge(PlanStep::Query).unwrap_err();
        assert_eq!(err.max_steps, 2);
        assert_eq!(err.executed_steps, 2);
        assert_eq!(err.attempted, PlanStep::Query);
        // The refused step is not recorded.
        assert_eq!(budget.executed_count(), 2);
    }

    #[test]
    fn plan_budget_request_above_cap_cannot_buy_extra_steps() {
        // STEPS 5 with a cap of 1 → only one step ever executes.
        let mut budget = PlanBudget::new(Some(5), 1);
        assert_eq!(budget.max_steps(), 1);
        assert!(budget.charge(PlanStep::Query).is_ok());
        assert!(budget.charge(PlanStep::RefineRetrieval).is_err());
    }

    // -----------------------------------------------------------------------
    // Grounding critique + single refine_retrieval retry (#1748).
    // -----------------------------------------------------------------------

    /// A funnel closure that counts calls and returns empty/non-empty per the
    /// script `[first, refined]`.
    fn scripted_funnel(
        script: Vec<bool>,
    ) -> (impl RetrievalFunnel, std::rc::Rc<std::cell::Cell<usize>>) {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let calls_inner = calls.clone();
        let funnel = move |_expanded: bool| -> RedDBResult<NarrowedSlice> {
            let n = calls_inner.get();
            calls_inner.set(n + 1);
            let non_empty = script.get(n).copied().unwrap_or(false);
            Ok(if non_empty {
                slice()
            } else {
                NarrowedSlice::default()
            })
        };
        (funnel, calls)
    }

    #[test]
    fn grounding_succeeds_on_first_pass_without_refining() {
        let (funnel, calls) = scripted_funnel(vec![true]);
        let outcome = ground_with_refine(&funnel).unwrap();
        assert_eq!(calls.get(), 1, "no refine when the first pass grounds");
        match outcome {
            GroundingOutcome::Grounded { refined, .. } => assert!(!refined),
            other => panic!("expected Grounded, got {other:?}"),
        }
    }

    #[test]
    fn empty_first_pass_triggers_exactly_one_refine_retrieval() {
        // First pass grounds nothing; the single refine retry grounds.
        let (funnel, calls) = scripted_funnel(vec![false, true]);
        let outcome = ground_with_refine(&funnel).unwrap();
        assert_eq!(calls.get(), 2, "exactly one refine re-funnel");
        match outcome {
            GroundingOutcome::Grounded { refined, .. } => assert!(refined),
            other => panic!("expected Grounded after refine, got {other:?}"),
        }
    }

    #[test]
    fn second_grounding_failure_returns_no_matching_sources_not_an_invented_answer() {
        // Both passes ground nothing → honest no-matching-sources; the funnel
        // is called exactly twice and never a third time.
        let (funnel, calls) = scripted_funnel(vec![false, false]);
        let outcome = ground_with_refine(&funnel).unwrap();
        assert_eq!(calls.get(), 2, "exactly one refine retry, then give up");
        assert_eq!(outcome, GroundingOutcome::NoMatchingSources);
    }

    #[test]
    fn plan_summary_includes_intent_and_query() {
        let plan = AskPlan {
            intent: AskIntent::Factual,
            query: Some("SELECT * FROM travelers WHERE passport = 'FDD-1'".to_string()),
            answer: String::new(),
            suggestion: Vec::new(),
            rationale: "lookup".to_string(),
        };
        let summary = plan.summary();
        assert!(summary.contains("intent=factual"));
        assert!(summary.contains("SELECT * FROM travelers"));
    }
}
