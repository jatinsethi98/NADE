//! The four tools a v1 agent can call.
//!
//! `search_mail`, `read_thread`, `write_note`, `draft_reply` — and nothing
//! else. `http_fetch` was cut (PLAN.md §Recorded deviations), and v1 takes no
//! outbound actions at all: a draft is a NADE draft and never a Gmail one.
//!
//! # Two rules that shape every tool here
//!
//! **A tool returns a projection, not a wire type.** `search::PAGE_SIZE` is 50
//! and `EngineConfig::max_tool_result_bytes` is 16 KiB. Fifty `ThreadSummary`s,
//! or one `ThreadDetail` carrying real message bodies, exceed that on the
//! *normal* path — so handing back the HTTP shape would deliver the model a
//! truncation envelope instead of results, every time, for ordinary mail.
//!
//! **A tool's output must be identical on every attempt.** The engine
//! guarantees each attempt at a step the same `effect_id` and byte-identical
//! input, and requires the tool to hold up its end: no clocks, no randomness,
//! no dependence on `is_replay`. Mutating tools write with
//! `insert … on conflict (id) do update` keyed on that `effect_id`, which is
//! what makes a re-execution after a crash collapse onto one row instead of
//! duplicating.

pub mod draft_reply;
pub mod read_thread;
pub mod search_mail;
pub mod write_note;

use std::sync::Arc;

use durable_agent::{Error as SdkError, Result as SdkResult, Tool, ToolCall};
use serde_json::Value;
use uuid::Uuid;

use crate::{state::Account, state::AppState};

pub use draft_reply::DraftReply;
pub use read_thread::ReadThread;
pub use search_mail::SearchMail;
pub use write_note::WriteNote;

/// Every tool name v1 knows. `API.md` §5.1: "v1's entire tool set".
///
/// Sorted, and `docs/contract/validate.py` carries the same list — a journal
/// naming anything outside it is a contract violation on both sides.
pub const V1_TOOLS: &[&str] = &["draft_reply", "read_thread", "search_mail", "write_note"];

/// What every tool needs: the server, whose mailbox it is reading, and whether
/// this agent's mutating calls are gated.
#[derive(Clone)]
pub struct ToolContext {
    pub state: AppState,
    pub account: Account,
    /// From `agents.approval_required`, itself seeded from the account's
    /// `settings.approval_required_default`.
    ///
    /// Baked in at construction so [`Tool::requires_approval`] is a pure
    /// function of the call, as the SDK requires: "a decision that flips
    /// between the two evaluations means the step was opened under different
    /// rules than it would run under".
    pub approval_required: bool,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("account", &self.account.id)
            .field("approval_required", &self.approval_required)
            .finish_non_exhaustive()
    }
}

/// Build the tools this agent is allowed, in a stable order.
///
/// The intersection with [`V1_TOOLS`] is the host-side half of `allowed_tools`:
/// `API.md` §5.1 says "the host enforces `allowed_tools` at dispatch regardless
/// of what the spec says", and the engine's `ToolSet` only ever dispatches what
/// it was constructed with. A name the agent asks for that does not exist is
/// dropped here rather than failing the run — an agent compiled by an older
/// build must still run.
#[must_use]
pub fn build(context: &ToolContext, allowed: &[String]) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    for name in V1_TOOLS {
        if !allowed.iter().any(|a| a == name) {
            continue;
        }
        let tool: Arc<dyn Tool> = match *name {
            "search_mail" => Arc::new(SearchMail::new(context.clone())),
            "read_thread" => Arc::new(ReadThread::new(context.clone())),
            "write_note" => Arc::new(WriteNote::new(context.clone())),
            "draft_reply" => Arc::new(DraftReply::new(context.clone())),
            _ => continue,
        };
        tools.push(tool);
    }
    tools
}

/// The `run_id` and `effect_id` the engine minted for this attempt.
///
/// Every mutating tool needs both, and a call that arrives without a
/// [`CallContext`](durable_agent::CallContext) cannot be idempotent — so it is
/// refused rather than being written under a fresh id, which is precisely the
/// duplicate-effect bug the whole protocol exists to prevent.
pub(crate) fn effect_identity(call: &ToolCall) -> SdkResult<(Uuid, Uuid)> {
    let context = call.context.as_ref().ok_or_else(|| {
        SdkError::tool(format!(
            "{} was called without a call context, so its effect could not be given a stable id",
            call.name
        ))
    })?;
    Ok((context.run_id.as_uuid(), context.effect_id))
}

/// Read a required string argument.
pub(crate) fn required_str<'a>(args: &'a Value, key: &str) -> SdkResult<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            SdkError::tool(format!(
                "`{key}` is required and must be a non-empty string"
            ))
        })
}

/// Read an optional string argument, treating blank as absent.
pub(crate) fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Resolve a model-supplied `thread_id` to one this account actually owns.
///
/// The column is bare `text` with no foreign key, so a compromised model could
/// otherwise file a note or a draft against a thread the mailbox does not have,
/// or one belonging to somebody else. The deep link would 404 either way, since
/// thread reads are account-scoped, so the impact is cosmetic. Leaving one
/// unchecked model-supplied identifier in a stored row is not.
///
/// A miss drops the field rather than failing the call: the note is still worth
/// saving, it just is not about a thread we know.
pub(crate) async fn owned_thread_id(
    state: &AppState,
    account_id: Uuid,
    thread_id: Option<&str>,
) -> Option<String> {
    let thread_id = thread_id?;
    let found: Option<i32> = sqlx::query_scalar(
        "select 1 from threads where account_id = $1 and thread_id = $2 limit 1",
    )
    .bind(account_id)
    .bind(thread_id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    found.map(|_| thread_id.to_owned())
}

/// Cap a string, on a character boundary, with a marker.
pub(crate) fn cap(text: &str, limit: usize) -> String {
    super::fence::truncate_bytes(text, limit).0
}

/// A subject is sender-controlled and, unlike a snippet, is not capped upstream:
/// `mail::parse::MAX_SUBJECT` allows 4 000 **characters**, which is ~16 KB of
/// astral codepoints - by itself enough to exceed the engine's result cap and
/// destroy every other field in the same result.
pub(crate) const MAX_SUBJECT_BYTES: usize = 300;

/// Addresses and display names are sender-controlled too.
pub(crate) const MAX_ADDRESS_BYTES: usize = 200;

/// A thread addressed to a hundred people does not need all hundred in a prompt.
pub(crate) const MAX_RECIPIENTS_SHOWN: usize = 10;

#[cfg(test)]
mod tests;
