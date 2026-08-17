//! The append-only run log.
//!
//! The contract an implementation must meet, and the vocabulary of entry kinds,
//! are documented on the [`Journal`] trait — this module is private and its
//! items are re-exported from the crate root, so trait-level docs are the ones
//! a reader actually sees.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::ids::{RunId, Seq};
use crate::message::{StopReason, TokenUsage, ToolCall};
use crate::run::{Decision, FailureReason, RunInput, RunStatus};

/// One durable fact about a run.
///
/// Shaped to match the NADE `run_journal` table one column at a time:
/// `(run_id, seq, kind text, payload jsonb, created_at timestamptz)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Position in the run's log, starting at 1 with no gaps.
    pub seq: Seq,
    /// What kind of fact this is.
    pub kind: EntryKind,
    /// The fact itself. One of the payload types in this module, as JSON.
    pub payload: serde_json::Value,
    /// When the engine created it. Not necessarily when it committed.
    pub created_at: DateTime<Utc>,
}

impl Entry {
    /// Build an entry, serialising a typed payload.
    pub fn new<P: Serialize>(seq: Seq, kind: EntryKind, payload: &P) -> Result<Self> {
        Ok(Self {
            seq,
            kind,
            payload: serde_json::to_value(payload)?,
            created_at: Utc::now(),
        })
    }

    /// Read the payload back as one of this module's payload types.
    pub fn payload_as<P: DeserializeOwned>(&self, run: RunId) -> Result<P> {
        serde_json::from_value(self.payload.clone()).map_err(|err| Error::CorruptJournal {
            run,
            message: format!(
                "entry {} ({}) has an unreadable payload: {err}",
                self.seq, self.kind
            ),
        })
    }
}

/// The kind tag on a journal entry.
///
/// Serialises as the bare string that goes in the `kind` column. Unrecognised
/// tags round-trip through [`EntryKind::Other`] rather than failing to parse,
/// so a journal written by a newer version can still be read (and, importantly,
/// still fails loudly at the point where its meaning would matter).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// The run's input, recorded once at the start.
    RunStarted,
    /// One turn from the model.
    ModelResponse,
    /// A tool is about to run.
    StepStarted,
    /// A tool finished; its result is capped and recorded.
    StepDone,
    /// A gated call is waiting on a human.
    ApprovalRequested,
    /// The human answered.
    ApprovalResolved,
    /// A tool parked the run on a timer.
    RunWaiting,
    /// The timer fired.
    RunWoken,
    /// A cap was breached.
    CapBreached,
    /// The run reached a terminal state.
    RunEnded,
    /// A tag this version does not know.
    Other(String),
}

impl EntryKind {
    /// Parse a stored tag. Unrecognised tags become [`EntryKind::Other`].
    pub fn from_tag(tag: &str) -> Self {
        match tag {
            "run_started" => Self::RunStarted,
            "model_response" => Self::ModelResponse,
            "step_started" => Self::StepStarted,
            "step_done" => Self::StepDone,
            "approval_requested" => Self::ApprovalRequested,
            "approval_resolved" => Self::ApprovalResolved,
            "run_waiting" => Self::RunWaiting,
            "run_woken" => Self::RunWoken,
            "cap_breached" => Self::CapBreached,
            "run_ended" => Self::RunEnded,
            other => Self::Other(other.to_string()),
        }
    }

    /// The tag as stored.
    pub fn as_str(&self) -> &str {
        match self {
            Self::RunStarted => "run_started",
            Self::ModelResponse => "model_response",
            Self::StepStarted => "step_started",
            Self::StepDone => "step_done",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalResolved => "approval_resolved",
            Self::RunWaiting => "run_waiting",
            Self::RunWoken => "run_woken",
            Self::CapBreached => "cap_breached",
            Self::RunEnded => "run_ended",
            Self::Other(s) => s,
        }
    }
}

impl FromStr for EntryKind {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::from_tag(s))
    }
}

impl std::fmt::Display for EntryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EntryKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EntryKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(EntryKind::from_tag(&raw))
    }
}

/// Payload of [`EntryKind::RunStarted`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunStarted {
    /// The input the run was started with. Replay uses this, not the argument
    /// passed to a later [`Engine::run`](crate::Engine::run) call.
    pub input: RunInput,
}

/// Payload of [`EntryKind::ModelResponse`].
///
/// Recorded so replay never pays for a turn twice: the conversation is rebuilt
/// from the journal, not from the provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Which turn this was, counting from 1.
    pub turn: u32,
    /// The model's prose, verbatim and untrimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The tool calls, with ids already normalised.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// What the turn cost.
    pub usage: TokenUsage,
}

/// Payload of [`EntryKind::StepStarted`].
///
/// **This entry is the fence.** Once it has committed, the engine must assume
/// the tool's side effect may exist, and any later replay re-executes the step
/// rather than skipping it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepStarted {
    /// The step's identity — the sequence of the entry that opened it.
    pub step_seq: Seq,
    /// The model's id for the call this answers to.
    pub call_id: String,
    /// The tool being run.
    pub tool: String,
    /// The arguments, verbatim.
    ///
    /// The full arguments are recorded, not just their hash, so replay is
    /// self-contained: it never has to reach back into an earlier entry to
    /// reconstruct the call.
    pub args: serde_json::Value,
    /// `sha256:<hex>` over the canonical JSON of `args`.
    pub args_hash: String,
    /// The id the effect must be keyed on.
    pub effect_id: Uuid,
    /// Which attempt this is, counting from 1.
    pub attempt: u32,
    /// When the step was **opened**, which is attempt 1's dispatch time.
    ///
    /// Reproduced identically on every retry so a tool's
    /// [`CallContext::opened_at`](crate::CallContext::opened_at) does not drift
    /// between attempts at one step.
    pub opened_at: DateTime<Utc>,
    /// [`tool_fingerprint`](crate::tool_fingerprint) of the implementation this
    /// step was opened under, or `None` when the model named a tool the engine
    /// does not have.
    ///
    /// Checked before every re-dispatch: a step is never executed under an
    /// implementation other than the one it was opened under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_fingerprint: Option<String>,
}

/// Payload of [`EntryKind::StepDone`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepDone {
    /// The step this closes.
    pub step_seq: Seq,
    /// The model's id for the call.
    pub call_id: String,
    /// The tool that ran.
    pub tool: String,
    /// The result, already passed through the size cap.
    pub result: serde_json::Value,
    /// Whether `result` is a structured error rather than a value.
    pub is_error: bool,
    /// Whether the size cap replaced the result with a truncation envelope.
    pub truncated: bool,
    /// Set when the tool asked to park the run, via
    /// [`control::wait_until`](crate::control::wait_until).
    ///
    /// **This is where a wait becomes durable**, not the [`RunWaiting`] entry
    /// that follows it. Completing the step and recording the pause are one
    /// committed fact, so a process that dies between them cannot lose the
    /// delay — which is exactly what a separate entry would allow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<DateTime<Utc>>,
}

/// Payload of [`EntryKind::ApprovalRequested`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequested {
    /// The step's identity, and the key a resolution must quote back. See
    /// [`Resolution`](crate::Resolution).
    pub step_seq: Seq,
    /// The model's id for the call.
    pub call_id: String,
    /// The tool the model wants to run.
    pub tool: String,
    /// The arguments, verbatim.
    pub args: serde_json::Value,
    /// `sha256:<hex>` over the canonical JSON of `args`.
    pub args_hash: String,
    /// The id the effect will be keyed on if this is approved.
    pub effect_id: Uuid,
    /// When the engine asked. Doubles as the step's
    /// [`opened_at`](crate::CallContext::opened_at).
    pub requested_at: DateTime<Utc>,
    /// When it stops being approvable, if a TTL is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// [`tool_fingerprint`](crate::tool_fingerprint) of the implementation the
    /// human is being asked about.
    ///
    /// Checked again at execution time: a redeployment that changes what
    /// `tool` does invalidates an approval taken for the old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_fingerprint: Option<String>,
}

/// Payload of [`EntryKind::ApprovalResolved`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalResolved {
    /// The step this settles.
    pub step_seq: Seq,
    /// The answer.
    pub decision: Decision,
    /// When it was settled.
    pub resolved_at: DateTime<Utc>,
    /// Why, when the engine decided rather than a human — currently only
    /// `"ttl_expired"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Payload of [`EntryKind::RunWaiting`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunWaiting {
    /// The step whose tool asked for the pause.
    pub step_seq: Seq,
    /// The earliest instant the host should resume at.
    pub wake_at: DateTime<Utc>,
}

/// Payload of [`EntryKind::RunWoken`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunWoken {
    /// When the timer actually fired.
    pub woken_at: DateTime<Utc>,
}

/// Payload of [`EntryKind::CapBreached`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapBreached {
    /// Which cap, and by how much.
    pub reason: FailureReason,
}

/// Payload of [`EntryKind::RunEnded`].
///
/// Carries enough to reconstruct the terminal [`RunOutcome`](crate::RunOutcome)
/// without re-deriving it, so a duplicate call on a finished run is a pure read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunEnded {
    /// The terminal state.
    pub status: RunStatus,
    /// The model's closing prose, trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// The cap that ended it, for [`RunStatus::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<FailureReason>,
    /// Steps executed over the run's life.
    pub steps: u32,
    /// Tokens spent over the run's life.
    pub usage: TokenUsage,
}

/// The durable, append-only log of a run: the engine's recovery log and the
/// host's UI log, deliberately the same thing.
///
/// # Storage contract
///
/// An implementation must behave like a table with `primary key (run_id, seq)`:
///
/// * **Append-only.** Entries are never updated or deleted.
/// * **Durable before return.** [`append`](Journal::append) must not return `Ok`
///   until the entry would survive a process death. The whole exactly-once
///   argument rests on this.
/// * **Unique.** A second append at a sequence that already exists must fail
///   with [`Error::SeqConflict`], not overwrite. That is what stops two workers
///   from driving one run into two different futures.
/// * **Ordered.** [`load`](Journal::load) returns entries sorted by `seq`,
///   ascending.
///
/// # Entry vocabulary
///
/// | kind | meaning | payload |
/// |---|---|---|
/// | `run_started` | the run's input, recorded once | [`RunStarted`] |
/// | `model_response` | one turn from the model, with normalised call ids | [`ModelResponse`] |
/// | `step_started` | a tool is about to run; **the effect may happen after this** | [`StepStarted`] |
/// | `step_done` | the tool's result, capped | [`StepDone`] |
/// | `approval_requested` | a gated call is waiting on a human | [`ApprovalRequested`] |
/// | `approval_resolved` | the human's answer | [`ApprovalResolved`] |
/// | `run_waiting` | a tool parked the run on a timer | [`RunWaiting`] |
/// | `run_woken` | the timer fired | [`RunWoken`] |
/// | `cap_breached` | a cap was hit; the run is about to fail | [`CapBreached`] |
/// | `run_ended` | the terminal state, recorded once | [`RunEnded`] |
///
/// A **step** is identified by its `step_seq`: the sequence of the entry that
/// opened it, either `step_started` or `approval_requested`. Every later entry
/// about that step carries the same `step_seq`, and its
/// [`effect_id`](crate::effect_id) is derived from it.
#[async_trait]
pub trait Journal: Send + Sync {
    /// Append `entry` at `entry.seq`, durably.
    ///
    /// Returns the sequence actually committed, which must equal `entry.seq`.
    /// Fails with [`Error::SeqConflict`] if that sequence is already taken.
    async fn append(&self, run: RunId, entry: Entry) -> Result<Seq>;

    /// Every entry for `run`, ordered by `seq` ascending. Empty for an unknown
    /// run — that is how the engine recognises a fresh one.
    async fn load(&self, run: RunId) -> Result<Vec<Entry>>;
}

#[async_trait]
impl<T: Journal + ?Sized> Journal for Arc<T> {
    async fn append(&self, run: RunId, entry: Entry) -> Result<Seq> {
        (**self).append(run, entry).await
    }

    async fn load(&self, run: RunId) -> Result<Vec<Entry>> {
        (**self).load(run).await
    }
}

#[async_trait]
impl<T: Journal + ?Sized> Journal for Box<T> {
    async fn append(&self, run: RunId, entry: Entry) -> Result<Seq> {
        (**self).append(run, entry).await
    }

    async fn load(&self, run: RunId) -> Result<Vec<Entry>> {
        (**self).load(run).await
    }
}
