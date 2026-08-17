//! The crate's error type.
//!
//! Two rules keep the taxonomy honest:
//!
//! * **Infrastructure failures are `Err`.** A journal that will not commit or a
//!   model endpoint that will not answer is the host's problem: it should retry
//!   the job, and the run stays exactly where the journal says it is.
//! * **Policy failures are outcomes.** Cap breaches end the run with
//!   [`RunOutcome::Failed`](crate::RunOutcome::Failed) and are journaled, because
//!   retrying would not help.

use uuid::Uuid;

use crate::ids::{RunId, Seq};
use crate::run::Decision;

/// A boxed, thread-safe source error.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The crate's result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong inside the engine or its collaborators.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The language model could not be reached, or answered unusably.
    ///
    /// The engine never converts this into a failed run — the host decides
    /// whether to retry.
    #[error("llm error: {message}")]
    Llm {
        /// Human-readable description.
        message: String,
        /// The underlying cause, if any.
        #[source]
        source: Option<BoxError>,
    },

    /// The journal could not be read or appended to.
    ///
    /// Because the journal *is* the recovery log, a failed append aborts the
    /// call: whatever committed before it stays committed and replay picks up
    /// from there.
    #[error("journal error: {message}")]
    Journal {
        /// Human-readable description.
        message: String,
        /// The underlying cause, if any.
        #[source]
        source: Option<BoxError>,
    },

    /// A tool refused to do its job. Reported back to the model as a structured
    /// tool result rather than ending the run.
    #[error("tool error: {message}")]
    Tool {
        /// Human-readable description.
        message: String,
        /// The underlying cause, if any.
        #[source]
        source: Option<BoxError>,
    },

    /// Another writer already committed this sequence number for this run.
    ///
    /// Mirrors the `primary key (run_id, seq)` on the Postgres journal: it is
    /// how two workers racing on one run discover each other.
    #[error("journal sequence conflict on run {run}: seq {seq} already exists")]
    SeqConflict {
        /// The run being written.
        run: RunId,
        /// The sequence number that was already taken.
        seq: Seq,
    },

    /// [`Engine::new`](crate::Engine::new) was handed two tools with one name.
    #[error("duplicate tool name: {0}")]
    DuplicateTool(String),

    /// `resume` was called with an approval decision, but the run is not
    /// waiting for one.
    #[error("run {0} has no pending approval")]
    NoPendingApproval(RunId),

    /// `resume` was called with [`Resolution::Timer`](crate::Resolution::Timer),
    /// but the run is not parked on a timer.
    #[error("run {0} is not waiting on a timer")]
    NotWaiting(RunId),

    /// The resolution names a step that is not the one the run is parked on.
    ///
    /// A [`Resolution`](crate::Resolution) identifies the step it settles, so a
    /// stale or misrouted delivery is refused instead of being applied to
    /// whatever happens to be open. **Nothing was appended and nothing was
    /// executed.**
    #[error(
        "run {run}: resolution names step {got}, but the run is parked on \
         {}",
        match expected { Some(seq) => format!("step {seq}"), None => "nothing".to_string() }
    )]
    StepMismatch {
        /// The run being resolved.
        run: RunId,
        /// The step the run is actually parked on, if any.
        expected: Option<Seq>,
        /// The step the resolution named.
        got: Seq,
    },

    /// The resolution names a step whose decision is already on record.
    ///
    /// The idempotent answer to a re-delivered decision: **nothing was appended
    /// and nothing was executed**, and the recorded `decision` is reported back.
    /// A caller re-delivering a decision should treat this as success — it means
    /// an earlier delivery already won — and call
    /// [`Engine::run`](crate::Engine::run) if it needs the run carried forward.
    ///
    /// This is the SDK-level equivalent of a consumed single-use approval token.
    #[error("run {run}: step {step_seq} was already resolved as '{}'", decision.as_str())]
    AlreadyResolved {
        /// The run being resolved.
        run: RunId,
        /// The step whose decision is already recorded.
        step_seq: Seq,
        /// The decision that is on record.
        decision: Decision,
    },

    /// The tool a step was opened under is not the tool the engine would run
    /// now.
    ///
    /// Raised when a step's recorded tool fingerprint does not match the
    /// current implementation's, or when a step opened *without* an approval
    /// gate would now require one. Executing either way would run a different
    /// thing under an identity — and an
    /// [`effect_id`](crate::effect_id) — that was minted for something else, so
    /// the engine refuses and the host must decide.
    #[error("run {run}: step {step_seq} was opened under a different '{tool}': {detail}")]
    ToolChanged {
        /// The run whose step cannot be re-dispatched.
        run: RunId,
        /// The step in question.
        step_seq: Seq,
        /// The tool named by the step.
        tool: String,
        /// What differs.
        detail: String,
    },

    /// An interrupted step belongs to a tool that declared itself unsafe to
    /// re-execute, so the engine will not run it a second time.
    ///
    /// See [`ReplayPolicy::Halt`](crate::ReplayPolicy::Halt). The `effect_id` is
    /// quoted so the host can look the effect up in its own outbox and settle
    /// the question the engine cannot.
    #[error(
        "run {run}: step {step_seq} ('{tool}') may or may not have completed, and \
         the tool forbids a blind retry; reconcile effect {effect_id}"
    )]
    AmbiguousEffect {
        /// The run that cannot proceed.
        run: RunId,
        /// The step whose outcome is unknown.
        step_seq: Seq,
        /// The tool that forbade the retry.
        tool: String,
        /// The id the effect would have been keyed on.
        effect_id: Uuid,
    },

    /// The journal contains something the engine cannot interpret — a truncated
    /// write, a payload from an incompatible version, or hand-editing.
    #[error("corrupt journal for run {run}: {message}")]
    CorruptJournal {
        /// The run whose journal is unreadable.
        run: RunId,
        /// What was wrong with it.
        message: String,
    },

    /// A value could not be encoded to or decoded from JSON.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Build an [`Error::Llm`] from a message.
    pub fn llm(message: impl Into<String>) -> Self {
        Self::Llm {
            message: message.into(),
            source: None,
        }
    }

    /// Build an [`Error::Llm`] that wraps a cause.
    pub fn llm_source(message: impl Into<String>, source: impl Into<BoxError>) -> Self {
        Self::Llm {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Build an [`Error::Journal`] from a message.
    pub fn journal(message: impl Into<String>) -> Self {
        Self::Journal {
            message: message.into(),
            source: None,
        }
    }

    /// Build an [`Error::Journal`] that wraps a cause.
    pub fn journal_source(message: impl Into<String>, source: impl Into<BoxError>) -> Self {
        Self::Journal {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Build an [`Error::Tool`] from a message.
    ///
    /// This is the error a [`Tool`](crate::Tool) implementation normally
    /// returns; the engine turns it into a structured tool result the model can
    /// read and recover from.
    pub fn tool(message: impl Into<String>) -> Self {
        Self::Tool {
            message: message.into(),
            source: None,
        }
    }

    /// Build an [`Error::Tool`] that wraps a cause.
    pub fn tool_source(message: impl Into<String>, source: impl Into<BoxError>) -> Self {
        Self::Tool {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// A short, stable machine code for this error.
    ///
    /// Used in the structured tool results handed back to the model, so it is
    /// part of the crate's observable contract.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Llm { .. } => "llm_error",
            Self::Journal { .. } => "journal_error",
            Self::Tool { .. } => "tool_error",
            Self::SeqConflict { .. } => "seq_conflict",
            Self::DuplicateTool(_) => "duplicate_tool",
            Self::NoPendingApproval(_) => "no_pending_approval",
            Self::NotWaiting(_) => "not_waiting",
            Self::StepMismatch { .. } => "step_mismatch",
            Self::AlreadyResolved { .. } => "already_resolved",
            Self::ToolChanged { .. } => "tool_changed",
            Self::AmbiguousEffect { .. } => "ambiguous_effect",
            Self::CorruptJournal { .. } => "corrupt_journal",
            Self::Json(_) => "json_error",
        }
    }
}
