//! A generic agent engine: a tool-calling loop that survives being killed.
//!
//! Three traits describe the world — [`Llm`] talks to a model, [`Tool`] does
//! something, [`Journal`] remembers — and [`Engine`] drives them. Nothing here
//! knows what your agent is for, which provider you use, or which database you
//! keep the journal in.
//!
//! # The problem this crate exists to solve
//!
//! An agent that only computes can be retried freely. An agent that *acts* —
//! writes a row, files a draft, sends a request — cannot: a process killed
//! halfway through a run leaves the world changed and the engine with no memory
//! of it. Retrying then does the thing twice.
//!
//! # The journal-before-effect protocol
//!
//! Every tool step is fenced by two durable writes:
//!
//! 1. append `step_started { step_seq, tool, args, args_hash, effect_id }` and
//!    **let it commit**;
//! 2. execute the tool;
//! 3. append `step_done { step_seq, result }`.
//!
//! Replay reads the journal and classifies each step:
//!
//! | journal shows | replay does |
//! |---|---|
//! | `step_started` and `step_done` | skips it — the result is already known |
//! | `step_started`, no `step_done` | **runs it again** |
//! | neither | runs it for the first time |
//!
//! The middle row is the interesting one. After a crash the engine cannot tell
//! whether the effect landed before the process died, so it assumes the worst
//! and runs the step again.
//!
//! # What that costs you: the idempotency contract
//!
//! Re-execution is only safe because every attempt at a step is handed the same
//! identifier:
//!
//! ```text
//! effect_id = uuid5(EFFECT_NAMESPACE, "<run-id>:<step-seq>")
//! ```
//!
//! available to the tool as [`ToolCall::effect_id`] and to the host as the free
//! function [`effect_id`]. **A consumer must write its side effects keyed on
//! that id, with an upsert.** A `note` row, a queued webhook, an outbound
//! request — whatever the effect is, its identity must come from the engine and
//! not from a fresh `uuid4()` or an auto-increment column. Get that wrong and
//! the second attempt writes a second row; get it right and the two attempts
//! collapse into one.
//!
//! Effects the crate cannot make idempotent for you — anything with no natural
//! key, like an email actually leaving a server — belong behind
//! [`Tool::requires_approval`], where a human is the fence.
//!
//! # The approval gate
//!
//! When [`Tool::requires_approval`] returns `true` the engine appends
//! `approval_requested`, returns [`RunOutcome::PendingApproval`] with everything
//! the host needs to build its own approval record, and **executes nothing**.
//! The only route from there to execution is [`Engine::resume`] with
//! [`Resolution::Approve`]. Calling [`Engine::run`] again does not open that
//! door, however many times it is called.
//!
//! # States
//!
//! ```text
//! queued → running → done | failed
//! running → pending_approval --approve--> queued → running
//!                            --skip----->  skipped
//!                            --expire--->  expired
//! running → waiting(wake_at) --timer---->  running
//! ```
//!
//! `queued` is the host's: a run that exists in its tables but has not been
//! handed to the engine. Everything else is reported as a [`RunOutcome`].
//!
//! # Caps
//!
//! [`EngineConfig`] bounds three things, and a breach ends the run as
//! [`RunOutcome::Failed`] with the breach journaled:
//!
//! * [`max_steps`](EngineConfig::max_steps) — how many tool steps may be
//!   opened, which is also what stops a model that loops on one tool forever;
//! * [`token_budget`](EngineConfig::token_budget) — total spend across turns;
//! * [`max_tool_result_bytes`](EngineConfig::max_tool_result_bytes) — the size
//!   of a single result, past which it is replaced by an explicit truncation
//!   envelope rather than quietly shortened.
//!
//! Transport problems are not cap breaches: an unreachable model or a journal
//! that will not commit comes back as [`Err`], leaves the run where its journal
//! says it is, and is the host's to retry.
//!
//! # What a consumer must guarantee
//!
//! 1. **Durability.** [`Journal::append`] returns `Ok` only once the entry would
//!    survive a crash.
//! 2. **Uniqueness.** A second append at an existing sequence fails with
//!    [`Error::SeqConflict`] instead of overwriting.
//! 3. **Idempotent effects**, keyed on [`ToolCall::effect_id`], upserted.
//! 4. **One driver per run.** Two workers calling [`Engine::run`] on one run id
//!    at the same time will collide on the journal's primary key; the host
//!    should still hold a lease so that collision is the backstop and not the
//!    plan.
//!
//! # Example
//!
//! All three traits, and a run that calls a tool and then answers.
//!
//! ```
//! use std::sync::{Arc, Mutex};
//!
//! use async_trait::async_trait;
//! use nade_agent_sdk::{
//!     ChatRequest, ChatResponse, Engine, EngineConfig, Entry, Error, Journal, Llm, Result,
//!     RunId, RunStatus, Seq, Tool, ToolCall,
//! };
//! use serde_json::{json, Value};
//!
//! struct Model;
//! #[async_trait]
//! impl Llm for Model {
//!     async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
//!         let tool_has_answered = req.messages.iter().any(|m| m.role() == "tool");
//!         Ok(if tool_has_answered {
//!             ChatResponse::text("noted").with_usage(30, 4)
//!         } else {
//!             ChatResponse::tool_call("c1", "write_note", json!({"body": "hello"}))
//!                 .with_usage(20, 8)
//!         })
//!     }
//! }
//!
//! struct WriteNote;
//! #[async_trait]
//! impl Tool for WriteNote {
//!     fn name(&self) -> &str { "write_note" }
//!     fn schema(&self) -> Value { json!({"type": "object"}) }
//!     async fn execute(&self, call: &ToolCall) -> Result<Value> {
//!         // A real tool would `insert … on conflict (id) do update` here,
//!         // keyed on exactly this id. That is what makes replay safe.
//!         Ok(json!({ "note_id": call.effect_id() }))
//!     }
//! }
//!
//! #[derive(Default)]
//! struct InMemoryJournal(Mutex<Vec<Entry>>);
//! #[async_trait]
//! impl Journal for InMemoryJournal {
//!     async fn append(&self, run: RunId, entry: Entry) -> Result<Seq> {
//!         let mut log = self.0.lock().expect("journal lock");
//!         if log.iter().any(|e| e.seq == entry.seq) {
//!             return Err(Error::SeqConflict { run, seq: entry.seq });
//!         }
//!         log.push(entry.clone());
//!         Ok(entry.seq)
//!     }
//!     async fn load(&self, _run: RunId) -> Result<Vec<Entry>> {
//!         Ok(self.0.lock().expect("journal lock").clone())
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<()> {
//! let engine = Engine::new(
//!     Model,
//!     vec![Arc::new(WriteNote) as Arc<dyn Tool>],
//!     InMemoryJournal::default(),
//!     EngineConfig::default(),
//! )?;
//!
//! let outcome = engine.run(RunId::new(), "write me a note").await?;
//! assert_eq!(outcome.status(), RunStatus::Done);
//! assert_eq!(outcome.stats().steps, 1);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

mod engine;
mod error;
mod ids;
mod journal;
mod llm;
mod message;
mod run;
mod tool;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use crate::engine::{Engine, EngineConfig};
pub use crate::error::{BoxError, Error, Result};
pub use crate::ids::{args_hash, effect_id, RunId, Seq, EFFECT_NAMESPACE};
pub use crate::journal::{
    ApprovalRequested, ApprovalResolved, CapBreached, Entry, EntryKind, Journal, ModelResponse,
    RunEnded, RunStarted, RunWaiting, RunWoken, StepDone, StepStarted,
};
pub use crate::llm::Llm;
pub use crate::message::{
    CallContext, ChatRequest, ChatResponse, Message, StopReason, TokenUsage, ToolCall, ToolSchema,
};
pub use crate::run::{
    ApprovalRequest, Decision, FailureReason, Resolution, RunInput, RunOutcome, RunStats, RunStatus,
};
pub use crate::tool::{control, is_truncated, Tool, ToolSet, TRUNCATED_KEY};

/// The README, compiled as a doctest so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
