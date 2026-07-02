//! Runtime VCS command parse + execute.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 5/10, issue #1626).
//! Houses the version-control command surface that PRD #1619 keeps out of the
//! central dispatch file, next to the rest of the VCS execution family:
//!
//! - **Parse helpers** — `strip_explain_prefix`, `parse_vcs_author`,
//!   `looks_like_commit_hash`, `strip_keyword_ci`, `parse_vcs_quoted`,
//!   `parse_vcs_atom`, `expect_vcs_end`, `parse_runtime_vcs_command`,
//!   `peek_top_level_as_of`, `walk_collections`, plus the private
//!   `RuntimeVcsCommand` / `RuntimeVcsResetMode` /
//!   `RuntimeVcsConflictResolution` intermediate enums.
//! - **Execute methods** — `execute_vcs_command`, `execute_vcs_diff_tvf`.
//!
//! `impl_core` re-exports the free fns still referenced from the central
//! dispatch so those call sites need no edits.
use super::execution_context::current_connection_id;
use super::impl_core::peek_top_level_as_of_with_table;
use super::*;

/// Heuristic: does the raw SQL reference a built-in whose output
/// varies by connection, clock, or randomness? Such queries must
/// skip the 30s result cache — see the call site for rationale.
///
/// ASCII case-insensitive substring match. False positives (the
/// token appears in a quoted string) only skip caching, which is
/// the conservative direction.
/// If `sql` starts with `EXPLAIN` followed by a generic explainable statement,
/// return the trimmed inner statement; otherwise `None`.
///
/// `EXPLAIN ALTER FOR CREATE TABLE ...` is a separate schema-diff
/// command handled inside the normal SQL parser, so we leave it
/// alone here. `EXPLAIN ASK` and `EXPLAIN MIGRATION` are also executable
/// read paths handled by the parser/runtime directly.
pub(crate) fn strip_explain_prefix(sql: &str) -> Option<&str> {
    let trimmed = sql.trim_start();
    let (head, rest) = trimmed.split_at(
        trimmed
            .find(|c: char| c.is_whitespace())
            .unwrap_or(trimmed.len()),
    );
    if !head.eq_ignore_ascii_case("EXPLAIN") {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    // Peek the next token; command-specific EXPLAIN forms defer to
    // the normal parser.
    let next_head_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    if rest[..next_head_end].eq_ignore_ascii_case("ALTER")
        || rest[..next_head_end].eq_ignore_ascii_case("ASK")
        || rest[..next_head_end].eq_ignore_ascii_case("MIGRATION")
    {
        return None;
    }
    Some(rest)
}

fn parse_vcs_author(raw: &str) -> crate::application::vcs::Author {
    let trimmed = raw.trim();
    if let Some((name, rest)) = trimmed.rsplit_once('<') {
        if let Some(email) = rest.strip_suffix('>') {
            return crate::application::vcs::Author {
                name: name.trim().to_string(),
                email: email.trim().to_string(),
            };
        }
    }
    crate::application::vcs::Author {
        name: trimmed.to_string(),
        email: String::new(),
    }
}

fn looks_like_commit_hash(value: &str) -> bool {
    value.len() >= 7 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) enum RuntimeVcsCommand {
    Checkpoint {
        message: String,
        author: Option<String>,
    },
    Checkout {
        target: String,
    },
    Reset {
        mode: RuntimeVcsResetMode,
        target: String,
    },
    Merge {
        branch: String,
    },
    CherryPick {
        commit: String,
    },
    Revert {
        commit: String,
    },
    ResolveConflict {
        key: String,
        resolution: RuntimeVcsConflictResolution,
    },
}

pub(crate) enum RuntimeVcsResetMode {
    Hard,
    Soft,
    Mixed,
}

pub(crate) enum RuntimeVcsConflictResolution {
    Ours,
    Theirs,
}

fn strip_keyword_ci<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let trimmed = input.trim_start();
    if trimmed.len() < keyword.len() || !trimmed[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &trimmed[keyword.len()..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn parse_vcs_quoted(input: &str) -> RedDBResult<(String, &str)> {
    let trimmed = input.trim_start();
    let rest = trimmed
        .strip_prefix('\'')
        .ok_or_else(|| RedDBError::Query("expected quoted string".to_string()))?;
    let end = rest
        .find('\'')
        .ok_or_else(|| RedDBError::Query("unterminated quoted string".to_string()))?;
    Ok((rest[..end].to_string(), rest[end + 1..].trim_start()))
}

fn parse_vcs_atom(input: &str) -> RedDBResult<(String, &str)> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('\'') {
        return parse_vcs_quoted(trimmed);
    }
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    if end == 0 {
        return Err(RedDBError::Query("expected VCS argument".to_string()));
    }
    Ok((trimmed[..end].to_string(), trimmed[end..].trim_start()))
}

fn expect_vcs_end(rest: &str) -> RedDBResult<()> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err(RedDBError::Query(format!(
            "unexpected token after VCS command: {}",
            rest.trim()
        )))
    }
}

pub(crate) fn parse_runtime_vcs_command(query: &str) -> Option<RedDBResult<RuntimeVcsCommand>> {
    let trimmed = query.trim_start();
    if let Some(rest) = strip_keyword_ci(trimmed, "CHECKPOINT") {
        return Some((|| {
            let (message, rest) = parse_vcs_quoted(rest)?;
            let rest = rest.trim_start();
            let author = if let Some(author_rest) = strip_keyword_ci(rest, "AUTHOR") {
                let (author, rest) = parse_vcs_quoted(author_rest)?;
                expect_vcs_end(rest)?;
                Some(author)
            } else {
                expect_vcs_end(rest)?;
                None
            };
            Ok(RuntimeVcsCommand::Checkpoint { message, author })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "CHECKOUT") {
        return Some((|| {
            let (target, rest) = parse_vcs_atom(rest)?;
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::Checkout { target })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "RESET") {
        if strip_keyword_ci(rest, "TENANT").is_some() {
            return None;
        }
        return Some((|| {
            let mut rest = rest.trim_start();
            let mode = if let Some(next) = strip_keyword_ci(rest, "HARD") {
                rest = next;
                RuntimeVcsResetMode::Hard
            } else if let Some(next) = strip_keyword_ci(rest, "SOFT") {
                rest = next;
                RuntimeVcsResetMode::Soft
            } else if let Some(next) = strip_keyword_ci(rest, "MIXED") {
                rest = next;
                RuntimeVcsResetMode::Mixed
            } else {
                RuntimeVcsResetMode::Mixed
            };
            let rest = strip_keyword_ci(rest, "TO")
                .ok_or_else(|| RedDBError::Query("expected TO in RESET".to_string()))?;
            let (target, rest) = parse_vcs_atom(rest)?;
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::Reset { mode, target })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "MERGE") {
        return Some((|| {
            let (branch, rest) = parse_vcs_atom(rest)?;
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::Merge { branch })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "CHERRY") {
        return Some((|| {
            let rest = strip_keyword_ci(rest, "PICK")
                .ok_or_else(|| RedDBError::Query("expected PICK in CHERRY PICK".to_string()))?;
            let (commit, rest) = parse_vcs_atom(rest)?;
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::CherryPick { commit })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "REVERT") {
        return Some((|| {
            let (commit, rest) = parse_vcs_atom(rest)?;
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::Revert { commit })
        })());
    }
    if let Some(rest) = strip_keyword_ci(trimmed, "RESOLVE") {
        // Only `RESOLVE CONFLICT …` is a VCS working-set verb. Plain RESOLVE /
        // `RESOLVE CONFIG …` (config secret resolution) must fall through to the
        // normal query surface — the `?` early-returns None for non-CONFLICT,
        // mirroring the RESET/TENANT disambiguation above.
        let rest = strip_keyword_ci(rest, "CONFLICT")?;
        return Some((|| {
            let (key, rest) = parse_vcs_quoted(rest)?;
            let rest = strip_keyword_ci(rest, "USING")
                .ok_or_else(|| RedDBError::Query("expected USING in RESOLVE".to_string()))?;
            let (resolution, rest) = if let Some(rest) = strip_keyword_ci(rest, "OURS") {
                (RuntimeVcsConflictResolution::Ours, rest)
            } else if let Some(rest) = strip_keyword_ci(rest, "THEIRS") {
                (RuntimeVcsConflictResolution::Theirs, rest)
            } else {
                return Err(RedDBError::Query(
                    "expected OURS or THEIRS in RESOLVE".to_string(),
                ));
            };
            expect_vcs_end(rest)?;
            Ok(RuntimeVcsCommand::ResolveConflict { key, resolution })
        })());
    }
    None
}

/// If the query is a plain SELECT whose top-level `TableQuery`
/// carries an `AS OF` clause, return a typed spec that the runtime
/// can feed to `vcs_resolve_as_of`. Returns `None` for any other
/// shape — joins, DML, EXPLAIN, or parse failures — so callers fall
/// back to the connection's regular MVCC snapshot. A cheap textual
/// prefilter skips the parse entirely when the source doesn't
/// mention `AS OF` / `as of`, keeping the autocommit hot path free.
fn peek_top_level_as_of(sql: &str) -> Option<crate::application::vcs::AsOfSpec> {
    peek_top_level_as_of_with_table(sql).map(|(spec, _)| spec)
}

pub(crate) fn walk_collections(expr: &QueryExpr, out: &mut Vec<String>) {
    match expr {
        QueryExpr::Table(t) => out.push(t.table.clone()),
        QueryExpr::Join(j) => {
            walk_collections(&j.left, out);
            walk_collections(&j.right, out);
        }
        QueryExpr::Insert(i) => out.push(i.table.clone()),
        QueryExpr::Update(u) => out.push(u.table.clone()),
        QueryExpr::Delete(d) => out.push(d.table.clone()),
        QueryExpr::QueueSelect(q) => out.push(q.queue.clone()),

        // DDL — include the target collection so DDL takes
        // `(Collection, X)` and blocks concurrent readers / writers
        // on the same collection. Other collections stay live
        // because Global is still IX.
        QueryExpr::CreateTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateCollection(q) => out.push(q.name.clone()),
        QueryExpr::CreateVector(q) => out.push(q.name.clone()),
        QueryExpr::DropTable(q) => out.push(q.name.clone()),
        QueryExpr::DropGraph(q) => out.push(q.name.clone()),
        QueryExpr::DropVector(q) => out.push(q.name.clone()),
        QueryExpr::DropDocument(q) => out.push(q.name.clone()),
        QueryExpr::DropKv(q) => out.push(q.name.clone()),
        QueryExpr::DropCollection(q) => out.push(q.name.clone()),
        QueryExpr::Truncate(q) => out.push(q.name.clone()),
        QueryExpr::AlterTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateIndex(q) => out.push(q.table.clone()),
        QueryExpr::DropIndex(q) => out.push(q.table.clone()),
        QueryExpr::CreateTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateMetric(q) => out.push(q.path.clone()),
        QueryExpr::AlterMetric(q) => out.push(q.path.clone()),
        QueryExpr::CreateSlo(q) => out.push(q.path.clone()),
        QueryExpr::DropTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateQueue(q) => out.push(q.name.clone()),
        QueryExpr::AlterQueue(q) => out.push(q.name.clone()),
        QueryExpr::DropQueue(q) => out.push(q.name.clone()),
        QueryExpr::QueueCommand(QueueCommand::Move {
            source,
            destination,
            ..
        }) => {
            out.push(source.clone());
            out.push(destination.clone());
        }
        QueryExpr::CreatePolicy(q) => out.push(q.table.clone()),
        QueryExpr::CreateView(q) => out.push(q.name.clone()),
        QueryExpr::DropView(q) => out.push(q.name.clone()),
        QueryExpr::RefreshMaterializedView(q) => out.push(q.name.clone()),

        // Vector / Hybrid / Graph / Path / commands reference
        // collections through fields whose shape varies; without a
        // uniform accessor we fall back to the global lock only —
        // benign because every runtime path still holds the global
        // mode.
        _ => {}
    }
}

/// Encode an in-house JSON value into a runtime `Value::Json`, falling back to
/// `Value::Null` on encode failure. Sole caller is `execute_vcs_diff_tvf`
/// (moved here from `impl_core` alongside it, issue #1626).
fn json_runtime_value(value: crate::json::Value) -> Value {
    crate::json::to_vec(&value)
        .map(Value::Json)
        .unwrap_or(Value::Null)
}

impl RedDBRuntime {
    pub(crate) fn execute_vcs_command(
        &self,
        query: &str,
        mode: QueryMode,
        cmd: RuntimeVcsCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::application::vcs::{
            Author, CheckoutInput, CheckoutTarget, CreateCommitInput, MergeInput, MergeOpts,
            ResetInput, ResetMode,
        };
        use crate::application::VcsUseCases;

        let conn_id = current_connection_id();
        let vcs = VcsUseCases::new(self);
        let default_author = || Author {
            name: "rql".to_string(),
            email: "rql@reddb.io".to_string(),
        };

        match cmd {
            RuntimeVcsCommand::Checkpoint { message, author } => {
                let author = author
                    .as_deref()
                    .map(parse_vcs_author)
                    .unwrap_or_else(default_author);
                let commit = vcs.commit(CreateCommitInput {
                    connection_id: conn_id,
                    message: message.clone(),
                    author,
                    committer: None,
                    amend: false,
                    allow_empty: true,
                })?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("checkpoint {}", commit.hash),
                    "vcs_checkpoint",
                ))
            }
            RuntimeVcsCommand::Checkout { target } => {
                let result = if target.starts_with("refs/tags/") {
                    vcs.checkout(CheckoutInput {
                        connection_id: conn_id,
                        target: CheckoutTarget::Tag(target.clone()),
                        force: false,
                    })
                } else if looks_like_commit_hash(&target) {
                    vcs.checkout(CheckoutInput {
                        connection_id: conn_id,
                        target: CheckoutTarget::Commit(target.clone()),
                        force: false,
                    })
                } else {
                    vcs.checkout(CheckoutInput {
                        connection_id: conn_id,
                        target: CheckoutTarget::Branch(target.clone()),
                        force: false,
                    })
                    .or_else(|_| {
                        vcs.checkout(CheckoutInput {
                            connection_id: conn_id,
                            target: CheckoutTarget::Commit(target.clone()),
                            force: false,
                        })
                    })
                }?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("checked out {}", result.name),
                    "vcs_checkout",
                ))
            }
            RuntimeVcsCommand::Reset {
                mode: reset_mode,
                target,
            } => {
                let mode = match reset_mode {
                    RuntimeVcsResetMode::Hard => ResetMode::Hard,
                    RuntimeVcsResetMode::Soft => ResetMode::Soft,
                    RuntimeVcsResetMode::Mixed => ResetMode::Mixed,
                };
                vcs.reset(ResetInput {
                    connection_id: conn_id,
                    target: target.clone(),
                    mode,
                })?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    "reset complete",
                    "vcs_reset",
                ))
            }
            RuntimeVcsCommand::Merge { branch } => {
                let outcome = vcs.merge(MergeInput {
                    connection_id: conn_id,
                    from: branch.clone(),
                    opts: MergeOpts::default(),
                    author: default_author(),
                })?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("merge complete; conflicts={}", outcome.conflicts.len()),
                    "vcs_merge",
                ))
            }
            RuntimeVcsCommand::CherryPick { commit } => {
                let outcome = vcs.cherry_pick(conn_id, &commit, default_author())?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "cherry-pick complete; conflicts={}",
                        outcome.conflicts.len()
                    ),
                    "vcs_cherry_pick",
                ))
            }
            RuntimeVcsCommand::Revert { commit } => {
                let reverted = vcs.revert(conn_id, &commit, default_author())?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("revert {}", reverted.hash),
                    "vcs_revert",
                ))
            }
            RuntimeVcsCommand::ResolveConflict { key, resolution } => {
                let value = match resolution {
                    RuntimeVcsConflictResolution::Ours => {
                        crate::json::Value::String("ours".to_string())
                    }
                    RuntimeVcsConflictResolution::Theirs => {
                        crate::json::Value::String("theirs".to_string())
                    }
                };
                vcs.conflict_resolve(&key, value)?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    "conflict resolved",
                    "vcs_resolve_conflict",
                ))
            }
        }
    }

    pub(crate) fn execute_vcs_diff_tvf(
        &self,
        args: &[String],
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::application::vcs::{DiffChange, DiffInput};
        use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
        use crate::storage::schema::Value;

        if !named_args.is_empty() {
            return Err(RedDBError::Query(
                "red.diff takes only positional commit arguments".to_string(),
            ));
        }
        if args.len() != 2 {
            return Err(RedDBError::Query(format!(
                "red.diff takes exactly 2 commit arguments, got {}",
                args.len()
            )));
        }

        let diff = self.vcs_diff(DiffInput {
            from: args[0].clone(),
            to: args[1].clone(),
            collection: None,
            summary_only: false,
        })?;
        let mut result = UnifiedResult::with_columns(vec![
            "from".into(),
            "to".into(),
            "collection".into(),
            "entity_id".into(),
            "change".into(),
            "before".into(),
            "after".into(),
        ]);
        for entry in diff.entries {
            let (change, before, after) = match entry.change {
                DiffChange::Added { after } => ("added", Value::Null, json_runtime_value(after)),
                DiffChange::Removed { before } => {
                    ("removed", json_runtime_value(before), Value::Null)
                }
                DiffChange::Modified { before, after } => (
                    "modified",
                    json_runtime_value(before),
                    json_runtime_value(after),
                ),
            };
            let mut record = UnifiedRecord::new();
            record.set("from", Value::text(diff.from.clone()));
            record.set("to", Value::text(diff.to.clone()));
            record.set("collection", Value::text(entry.collection));
            record.set("entity_id", Value::text(entry.entity_id));
            record.set("change", Value::text(change));
            record.set("before", before);
            record.set("after", after);
            result.push(record);
        }
        Ok(result)
    }
}
