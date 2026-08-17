//! A generic agent engine: a tool-calling loop that survives being killed.
//!
//! Three traits describe the world — [`Llm`] talks to a model, [`Tool`] does
//! something, [`Journal`] remembers — and [`Engine`] drives them. Nothing here
//! knows what your agent is for, which provider you use, or which database you
//! keep the journal in.
//!
//! # What this crate guarantees, exactly
//!
//! > **At-least-once execution with stable idempotency keys** — conditional on a
//! > durable journal and idempotent effects.
//!
//! Read that as three claims and one warning.
//!
//! * **At-least-once.** A tool step that was interrupted between its fence and
//!   its result is executed *again*. It is not exactly-once, and nothing that
//!   survives a process death can be: at the instant the process dies, whether
//!   the effect landed is a fact about the outside world that the engine has no
//!   record of.
//! * **Stable keys.** Every attempt at one step is handed the same
//!   [`effect_id`] and byte-identical arguments, so an effect written as an
//!   upsert on that key collapses two attempts into one row.
//! * **A single durable decision point.** The engine appends and *waits* before
//!   it acts, so the journal is never behind the world.
//! * **The warning:** exactly-once is something *you* achieve, by making effects
//!   idempotent under `effect_id`, and it is available only if
//!   [`Journal::append`] really is durable. This crate ships no durable journal
//!   — see [Durability is yours](#durability-is-yours).
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
//! Replay finishes what is recorded rather than starting anything over. A run
//! whose final model response committed is completed from that response, never
//! by asking the model again; a run whose approval was skipped is ended
//! `skipped`; a run whose cap breach was journaled is failed with that breach.
//! An absent `run_ended` means "finish what is recorded", not "start again".
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
//! Every attempt at one step also sees identical *input* — same arguments, same
//! [`CallContext`], including its [`opened_at`](CallContext::opened_at). The one
//! field that moves is [`ToolCall::is_replay`], and it is advisory: a tool whose
//! effect depends on it is not idempotent, whatever key it upserts on.
//!
//! # Approval is not idempotency
//!
//! [`Tool::requires_approval`] controls **authorisation**. It answers "is this
//! allowed to happen?" — once, before anything happens. It does not answer "has
//! this already happened?", and it cannot.
//!
//! An approved step is fenced and replayed exactly like an unapproved one. The
//! human says yes; `approval_resolved` commits; `step_started` commits; the tool
//! sends the email; the process dies before `step_done` commits. On the next
//! run the journal shows a step that started and never finished, and the engine
//! does the only safe thing it knows: it runs it again. **The email goes
//! twice.** A human at the gate changes nothing about that, because the gate was
//! passed before the ambiguity was created.
//!
//! So: an effect with no natural key needs an **outbox**, not an approval.
//! Inside one transaction, write the effect row keyed on `effect_id` *and* a
//! row saying "this needs sending"; deliver it from a sweeper that marks it
//! sent. Re-execution then rewrites the same two rows and the sweeper still
//! sends once. The approval decides *whether*; the outbox decides *how many
//! times*, and they are different questions.
//!
//! If you genuinely cannot key an effect, [`ReplayPolicy::Halt`] is the guard
//! rail: the engine refuses to blind-retry the step and stops the run with
//! [`FailureReason::AmbiguousEffect`], quoting the `effect_id` so a human can
//! reconcile. That is damage control, not a solution — it converts a silent
//! double-send into a loud stop.
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
//! Every resolution names the step it settles, by the `step_seq` quoted on the
//! [`ApprovalRequest`]. Without that binding a delayed duplicate of an old
//! decision is indistinguishable from a fresh decision about whatever the run is
//! parked on *now*, and the engine would settle the wrong step. A mismatch is
//! [`Error::StepMismatch`]; a re-delivery of a decision already on record is
//! [`Error::AlreadyResolved`], which appends nothing, executes nothing, and
//! should be read as success by whoever re-delivered it.
//!
//! # Durability is yours
//!
//! **This crate contains no durable journal**, and every crash-safety sentence
//! above is conditional on the one you supply. There is no fsync here, no WAL,
//! no transaction; the only implementation that ships is
//! `testing::MemoryJournal` (behind the `testing` feature), a `HashMap` behind a
//! mutex that is exactly as durable as the process.
//!
//! [`Journal::append`] returning `Ok` is a promise that the entry would survive
//! the process being killed at the next instruction. An implementation that
//! returns before the write is durable does not make the guarantee weaker — it
//! removes it, silently, from every run: the fence stops fencing, and a step
//! whose effect landed can come back as a step that was never opened. See the
//! [`Journal`] trait docs for what "durable" has to mean in practice.
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
//! The crate's guarantee is conditional on all four. Miss one and what is left
//! is a tool-calling loop with a nice log.
//!
//! 1. **Durability.** [`Journal::append`] returns `Ok` only once the entry would
//!    survive a crash. This is the load-bearing one.
//! 2. **Uniqueness.** A second append at an existing sequence fails with
//!    [`Error::SeqConflict`] instead of overwriting.
//! 3. **Idempotent effects**, keyed on [`ToolCall::effect_id`], upserted —
//!    including effects behind an approval gate, which the gate does nothing to
//!    protect.
//! 4. **One driver per run.** Two workers calling [`Engine::run`] on one run id
//!    at the same time will collide on the journal's primary key; the host
//!    should still hold a lease so that collision is the backstop and not the
//!    plan.
//!
//! Do those and you get exactly-once *effects* out of at-least-once execution.
//! Skip the upsert and you get at-least-once effects, which for a `send` is a
//! duplicate and for an `insert` is a duplicate row.
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
//! // NOT a durable journal — it is a `Vec` behind a mutex, and it makes none
//! // of this crate's crash-safety guarantees. A real one commits to storage
//! // before it returns. See the `Journal` trait docs.
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
pub use crate::tool::{
    control, is_truncated, tool_fingerprint, ReplayPolicy, Tool, ToolSet, TRUNCATED_KEY,
};

/// The README, compiled as a doctest so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
