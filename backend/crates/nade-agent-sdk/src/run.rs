//! The vocabulary of a run: what goes in, what comes out, and every state in
//! between.
//!
//! ```text
//! queued → running → done | failed
//! running → pending_approval --approve--> queued → running
//!                            --skip----->  skipped
//!                            --expire--->  expired
//! running → waiting(wake_at) --timer---->  running
//! ```
//!
//! `queued` belongs to the host — it is the state of a run that exists in the
//! host's tables but has not been handed to [`Engine::run`](crate::Engine::run)
//! yet. Everything from `running` onwards is what this crate reports.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{RunId, Seq};
use crate::message::{Message, TokenUsage, ToolCall};

/// What a run is started with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunInput {
    /// Instructions that frame the run, prepended as a system message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The opening conversation. Usually a single user message.
    #[serde(default)]
    pub messages: Vec<Message>,
}

impl RunInput {
    /// A run driven by one user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            system: None,
            messages: vec![Message::user(text)],
        }
    }

    /// Attach a system prompt. Chainable.
    #[must_use]
    pub fn with_system(mut self, text: impl Into<String>) -> Self {
        self.system = Some(text.into());
        self
    }

    /// The opening messages, system prompt first.
    pub(crate) fn seed(&self) -> Vec<Message> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        if let Some(system) = &self.system {
            out.push(Message::system(system.clone()));
        }
        out.extend(self.messages.iter().cloned());
        out
    }
}

impl From<&str> for RunInput {
    fn from(text: &str) -> Self {
        Self::user(text)
    }
}

impl From<String> for RunInput {
    fn from(text: String) -> Self {
        Self::user(text)
    }
}

/// The state a run is in, as a flat tag suitable for a database column.
///
/// Matches the `agent_runs.status` check constraint in the NADE schema, minus
/// `queued` (the host's own pre-engine state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Finished successfully.
    Done,
    /// Stopped by a cap breach.
    Failed,
    /// Parked on a human decision.
    PendingApproval,
    /// Parked on a timer.
    Waiting,
    /// A human declined the pending action.
    Skipped,
    /// The pending action was never decided in time.
    Expired,
}

impl RunStatus {
    /// Whether no further work will happen without host intervention.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Skipped | Self::Expired
        )
    }

    /// The tag as it appears in the journal and in a database column.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::PendingApproval => "pending_approval",
            Self::Waiting => "waiting",
            Self::Skipped => "skipped",
            Self::Expired => "expired",
        }
    }
}

/// Why a run ended in [`RunStatus::Failed`].
///
/// Cap breaches and one safety stop land here. Transport problems — an
/// unreachable model, a journal that will not commit — are returned as
/// [`Err`](crate::Error) so the host can retry the job instead of burning the
/// run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cap", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FailureReason {
    /// The run tried to take more tool steps than [`EngineConfig::max_steps`]
    /// allows.
    ///
    /// [`EngineConfig::max_steps`]: crate::EngineConfig::max_steps
    StepCapExceeded {
        /// The configured ceiling.
        limit: u32,
        /// Steps already taken when the next one was refused.
        taken: u32,
    },
    /// The run spent more tokens than [`EngineConfig::token_budget`] allows.
    ///
    /// [`EngineConfig::token_budget`]: crate::EngineConfig::token_budget
    TokenBudgetExceeded {
        /// The configured ceiling.
        limit: u64,
        /// Tokens spent when the breach was detected.
        spent: u64,
    },
    /// A step was interrupted between its fence and its result, and its tool
    /// declared itself unsafe to re-execute
    /// ([`ReplayPolicy::Halt`](crate::ReplayPolicy::Halt)).
    ///
    /// The engine did **not** run it a second time. Whether the effect landed is
    /// a question only the host can answer, by looking `effect_id` up in its own
    /// outbox.
    AmbiguousEffect {
        /// The tool whose outcome is unknown.
        tool: String,
        /// The step that was interrupted.
        step_seq: Seq,
        /// The id the effect would have been keyed on.
        effect_id: Uuid,
    },
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StepCapExceeded { limit, taken } => {
                write!(
                    f,
                    "step cap exceeded: {taken} steps taken, limit is {limit}"
                )
            }
            Self::TokenBudgetExceeded { limit, spent } => {
                write!(f, "token budget exceeded: {spent} spent, limit is {limit}")
            }
            Self::AmbiguousEffect {
                tool,
                step_seq,
                effect_id,
            } => {
                write!(
                    f,
                    "step {step_seq} ('{tool}') was interrupted and forbids a blind retry; \
                     reconcile effect {effect_id}"
                )
            }
        }
    }
}

/// Everything the host needs to build its own approval record.
///
/// Deliberately self-contained: the host never has to read the journal to
/// render an approval card, and the [`effect_id`](ApprovalRequest::effect_id)
/// is quoted here so a row can be pre-created before the human decides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The journal sequence that opened the gated step. Identifies the step for
    /// the rest of its life.
    pub step_seq: Seq,
    /// The tool the model wants to run.
    pub tool: String,
    /// The full call, with its [`CallContext`](crate::CallContext) attached.
    pub call: ToolCall,
    /// `sha256:<hex>` over the canonical JSON of the arguments.
    pub args_hash: String,
    /// `uuid5(run_id ‖ step_seq)` — the id the effect will be written under if
    /// this is approved.
    pub effect_id: Uuid,
    /// When the engine asked.
    pub requested_at: DateTime<Utc>,
    /// When the request stops being approvable, if
    /// [`EngineConfig::approval_ttl`](crate::EngineConfig::approval_ttl) is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Counters describing how much a run has consumed so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStats {
    /// Tool steps executed, including ones that errored or hit an unknown tool.
    pub steps: u32,
    /// Tokens spent across every model turn.
    pub usage: TokenUsage,
    /// The highest journal sequence committed for this run.
    pub last_seq: Seq,
}

/// Where a call to [`Engine::run`](crate::Engine::run) or
/// [`Engine::resume`](crate::Engine::resume) left the run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunOutcome {
    /// The model finished.
    Done {
        /// The run.
        run_id: RunId,
        /// The model's closing prose, trimmed. `None` when it said nothing.
        output: Option<String>,
        /// Consumption counters.
        stats: RunStats,
    },
    /// A gated tool call is waiting on a human. Nothing was executed.
    PendingApproval {
        /// The run.
        run_id: RunId,
        /// Everything needed to ask the human.
        request: Box<ApprovalRequest>,
        /// Consumption counters.
        stats: RunStats,
    },
    /// A tool parked the run on a timer.
    Waiting {
        /// The run.
        run_id: RunId,
        /// The step whose tool asked for the pause. Quote it back in
        /// [`Resolution::Timer`] so a stale wake-up cannot cut short a later,
        /// unrelated wait.
        step_seq: Seq,
        /// The earliest instant to resume at.
        wake_at: DateTime<Utc>,
        /// Consumption counters.
        stats: RunStats,
    },
    /// A cap was breached.
    Failed {
        /// The run.
        run_id: RunId,
        /// Which cap, and by how much.
        reason: FailureReason,
        /// Consumption counters.
        stats: RunStats,
    },
    /// A human declined the pending action.
    Skipped {
        /// The run.
        run_id: RunId,
        /// Consumption counters.
        stats: RunStats,
    },
    /// The pending action was never decided in time.
    Expired {
        /// The run.
        run_id: RunId,
        /// Consumption counters.
        stats: RunStats,
    },
}

impl RunOutcome {
    /// The run this describes.
    pub fn run_id(&self) -> RunId {
        match self {
            Self::Done { run_id, .. }
            | Self::PendingApproval { run_id, .. }
            | Self::Waiting { run_id, .. }
            | Self::Failed { run_id, .. }
            | Self::Skipped { run_id, .. }
            | Self::Expired { run_id, .. } => *run_id,
        }
    }

    /// The flat status tag.
    pub fn status(&self) -> RunStatus {
        match self {
            Self::Done { .. } => RunStatus::Done,
            Self::PendingApproval { .. } => RunStatus::PendingApproval,
            Self::Waiting { .. } => RunStatus::Waiting,
            Self::Failed { .. } => RunStatus::Failed,
            Self::Skipped { .. } => RunStatus::Skipped,
            Self::Expired { .. } => RunStatus::Expired,
        }
    }

    /// Consumption counters.
    pub fn stats(&self) -> RunStats {
        match self {
            Self::Done { stats, .. }
            | Self::PendingApproval { stats, .. }
            | Self::Waiting { stats, .. }
            | Self::Failed { stats, .. }
            | Self::Skipped { stats, .. }
            | Self::Expired { stats, .. } => *stats,
        }
    }

    /// Whether the run is finished for good.
    pub fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }

    /// The approval request, when the run is paused on one.
    pub fn approval(&self) -> Option<&ApprovalRequest> {
        match self {
            Self::PendingApproval { request, .. } => Some(request),
            _ => None,
        }
    }
}

/// A human's answer to a pending approval, or a timer firing.
///
/// The only way a gated tool ever runs. There is no other entry point: an
/// engine handed a call that [`Tool::requires_approval`](crate::Tool::requires_approval)
/// accepts will pause and return, no matter how many times
/// [`Engine::run`](crate::Engine::run) is called afterwards.
///
/// # Every resolution names the step it settles
///
/// A resolution is bound to a **step**, identified by `step_seq` — the journal
/// sequence of the entry that opened it. That binding is not decoration: without
/// it, a delayed duplicate of an old decision is indistinguishable from a fresh
/// decision about whatever the run happens to be parked on now, and the engine
/// would settle the wrong step. See [`ApprovalRequest::step_seq`] and
/// [`RunOutcome::Waiting::step_seq`], which are where a host gets the value.
///
/// [`Engine::resume`](crate::Engine::resume) rejects a resolution that does not
/// match:
///
/// | the run is | the resolution names | result |
/// |---|---|---|
/// | parked on step *N* | *N* | settled |
/// | parked on step *N* | *M* ≠ *N* | [`Error::StepMismatch`](crate::Error::StepMismatch) |
/// | anywhere | a step already decided | [`Error::AlreadyResolved`](crate::Error::AlreadyResolved) |
/// | not parked at all | anything | [`Error::NoPendingApproval`](crate::Error::NoPendingApproval) / [`Error::NotWaiting`](crate::Error::NotWaiting) |
///
/// # Composing with a host's own approval token
///
/// A host that issues its own single-use token per approval (NADE's
/// `approval_token`, one per feed item) should store `step_seq` beside it and
/// pass it back here. The token makes the *request* single-use; `step_seq` makes
/// the *decision* address exactly one step. Neither substitutes for the other,
/// and together the host never has to guess which step a token belongs to.
///
/// `effect_id` is derivable from the pair: `effect_id(run_id, step_seq)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case")]
pub enum Resolution {
    /// Run the pending call. Refused, and the run expired instead, if the
    /// approval's TTL has passed.
    Approve {
        /// The step this approves.
        step_seq: Seq,
    },
    /// Decline it. The run ends [`RunStatus::Skipped`] without executing the
    /// pending call.
    ///
    /// Steps that already completed earlier in the run are not undone — the
    /// engine has no notion of a compensating action. Declining stops what has
    /// not happened yet.
    Skip {
        /// The step this declines.
        step_seq: Seq,
    },
    /// Age it out — what an expiry sweep calls. The run ends
    /// [`RunStatus::Expired`] without executing.
    Expire {
        /// The step this ages out.
        step_seq: Seq,
    },
    /// Wake a run parked by [`control::wait_until`](crate::control::wait_until).
    Timer {
        /// The step whose tool parked the run, from
        /// [`RunOutcome::Waiting`].
        step_seq: Seq,
    },
}

impl Resolution {
    /// Approve the step opened at `step_seq`.
    pub const fn approve(step_seq: Seq) -> Self {
        Self::Approve { step_seq }
    }

    /// Decline the step opened at `step_seq`.
    pub const fn skip(step_seq: Seq) -> Self {
        Self::Skip { step_seq }
    }

    /// Age out the step opened at `step_seq`.
    pub const fn expire(step_seq: Seq) -> Self {
        Self::Expire { step_seq }
    }

    /// Fire the timer the step opened at `step_seq` parked the run on.
    pub const fn timer(step_seq: Seq) -> Self {
        Self::Timer { step_seq }
    }

    /// The step this resolution settles.
    pub const fn step_seq(&self) -> Seq {
        match self {
            Self::Approve { step_seq }
            | Self::Skip { step_seq }
            | Self::Expire { step_seq }
            | Self::Timer { step_seq } => *step_seq,
        }
    }

    /// The id the step's effect is keyed on, if this resolution lets it run.
    ///
    /// `None` for [`Skip`](Resolution::Skip) and [`Expire`](Resolution::Expire),
    /// which never execute anything, and for [`Timer`](Resolution::Timer),
    /// whose step has already run.
    pub fn effect_id(&self, run: RunId) -> Option<Uuid> {
        match self {
            Self::Approve { step_seq } => Some(crate::ids::effect_id(run, *step_seq)),
            _ => None,
        }
    }

    /// The decision this resolution records, for the three that settle an
    /// approval. `None` for [`Timer`](Resolution::Timer).
    pub(crate) const fn decision(&self) -> Option<Decision> {
        match self {
            Self::Approve { .. } => Some(Decision::Approve),
            Self::Skip { .. } => Some(Decision::Skip),
            Self::Expire { .. } => Some(Decision::Expire),
            Self::Timer { .. } => None,
        }
    }
}

/// How a pending approval was settled, as recorded in the journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Approved and executed.
    Approve,
    /// Declined by a human.
    Skip,
    /// Aged out, either by an explicit sweep or by the TTL check on approve.
    Expire,
}

impl Decision {
    /// The tag as it appears in the journal.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Skip => "skip",
            Self::Expire => "expire",
        }
    }
}
