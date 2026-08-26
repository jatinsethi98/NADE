//! Provider-neutral conversation types.
//!
//! Nothing here models a specific vendor's wire format. An adapter for a given
//! API is expected to translate in both directions; the shapes were chosen to
//! be the common denominator of the tool-calling APIs in the wild:
//!
//! * a turn from the model is **text, tool calls, or both**;
//! * a tool result is addressed back to the call it answers by `call_id`;
//! * usage is two counters, because that is all every provider agrees on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{RunId, Seq};

/// One message in the conversation the engine maintains.
///
/// Serialises with a `role` tag so a journal payload reads naturally:
/// `{"role":"user","text":"…"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// Instructions that frame the whole run.
    System {
        /// The instruction text.
        text: String,
    },
    /// Input from the human or the host.
    User {
        /// The message text.
        text: String,
    },
    /// A turn produced by the model: prose, tool calls, or both.
    Assistant {
        /// Prose, if the model emitted any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Tool calls, if the model requested any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    /// The answer to one tool call, fed back to the model.
    Tool {
        /// The [`ToolCall::id`] this answers.
        call_id: String,
        /// The tool that produced it.
        name: String,
        /// The (already size-capped) result, or a structured error.
        result: serde_json::Value,
        /// Whether `result` describes a failure rather than a value.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Message {
    /// A system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self::System { text: text.into() }
    }

    /// A user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self::User { text: text.into() }
    }

    /// An assistant message carrying only prose.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant {
            text: Some(text.into()),
            tool_calls: Vec::new(),
        }
    }

    /// The role tag, as it appears in serialised form.
    pub fn role(&self) -> &'static str {
        match self {
            Self::System { .. } => "system",
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::Tool { .. } => "tool",
        }
    }
}

/// A request for one turn from the model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// The conversation so far, oldest first.
    pub messages: Vec<Message>,
    /// The tools the model is allowed to call this turn.
    ///
    /// The engine only ever advertises tools it was constructed with — this is
    /// the host-enforced allowlist. A model that invents a name outside this
    /// list gets a structured error back, not an execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
    /// Optional model identifier, when the adapter serves more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional ceiling on the tokens the model may generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Optional sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// A tool as advertised to the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// The name the model must use to call it.
    pub name: String,
    /// What it does, in the model's terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// One turn produced by the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Prose, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool calls, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// What the turn cost.
    #[serde(default)]
    pub usage: TokenUsage,
}

impl ChatResponse {
    /// A final answer: prose, [`StopReason::EndTurn`], no tool calls.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
        }
    }

    /// A turn that requests exactly one tool call.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::tool_calls(vec![ToolCall::new(id, name, arguments)])
    }

    /// A turn that requests several tool calls at once.
    pub fn tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            text: None,
            tool_calls: calls,
            stop_reason: StopReason::ToolUse,
            usage: TokenUsage::default(),
        }
    }

    /// Attach token usage. Chainable.
    #[must_use]
    pub fn with_usage(mut self, input_tokens: u32, output_tokens: u32) -> Self {
        self.usage = TokenUsage {
            input_tokens,
            output_tokens,
        };
        self
    }

    /// Attach prose alongside whatever else the turn carries. Chainable.
    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// Why the model stopped generating.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    #[default]
    EndTurn,
    /// The model wants one or more tools run.
    ToolUse,
    /// The output ceiling was hit mid-answer.
    MaxTokens,
    /// A stop sequence matched.
    StopSequence,
    /// The provider filtered the output.
    ContentFilter,
    /// Anything a provider reports that does not map onto the above.
    Other(String),
}

/// Token counters for one turn.
///
/// Only the two counters every provider reports. Cache-read/cache-write
/// breakdowns are deliberately absent: they differ per vendor and belong in the
/// adapter, not in the budget the engine enforces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Tokens sent to the model.
    #[serde(default)]
    pub input_tokens: u32,
    /// Tokens generated by the model.
    #[serde(default)]
    pub output_tokens: u32,
}

impl TokenUsage {
    /// Input plus output, widened so a long run cannot overflow the total.
    pub fn total(&self) -> u64 {
        u64::from(self.input_tokens) + u64::from(self.output_tokens)
    }

    /// Add another turn's usage to this one, saturating rather than wrapping.
    pub fn add(&mut self, other: TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

/// A request from the model to run one tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's identifier for this call, used to address the result back.
    ///
    /// The engine normalises this before journaling anything: blank ids and
    /// ids the model reused within a run are replaced with a deterministic
    /// `call_<turn>_<index>`, so replay always resolves results to calls the
    /// same way.
    pub id: String,
    /// The tool name the model asked for. Not yet known to exist.
    pub name: String,
    /// The arguments, as the model produced them.
    ///
    /// Usually a JSON object, but not enforced: a model that emits a scalar or
    /// an array is passed through unchanged so the tool can reject it with a
    /// message the model can act on.
    pub arguments: serde_json::Value,
    /// Run context, filled in by the engine immediately before dispatch.
    ///
    /// `None` on calls that have not reached the engine yet — for instance one
    /// you construct by hand in a test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<CallContext>,
}

impl ToolCall {
    /// A call with no engine context attached.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            context: None,
        }
    }

    /// The deterministic id this call's side effect must be keyed on.
    ///
    /// `None` only if the call has not been dispatched by an engine. See
    /// [`effect_id`](crate::effect_id) for the contract it carries.
    pub fn effect_id(&self) -> Option<Uuid> {
        self.context.as_ref().map(|c| c.effect_id)
    }

    /// Whether this dispatch is a re-execution after a crash.
    ///
    /// `true` means an earlier attempt at this exact step committed its
    /// `step_started` entry but never its `step_done`, so the effect may or may
    /// not already exist.
    ///
    /// **This is the only field that differs between attempts at one step, and
    /// it is advisory.** Log it, skip an expensive re-read with it — but a tool
    /// whose *effect* depends on it is not idempotent, and re-execution will
    /// then produce something the first attempt did not. Upserting on
    /// [`ToolCall::effect_id`] is correct either way.
    pub fn is_replay(&self) -> bool {
        self.context.as_ref().is_some_and(|c| c.replay)
    }
}

/// What the engine knows about a call that the model does not.
///
/// Every field except [`replay`](CallContext::replay) is fixed when the step is
/// opened and reproduced verbatim on every later attempt, so two attempts at one
/// step are handed identical input. That is a precondition for the idempotency
/// contract, not a convenience: a tool cannot upsert deterministically if its
/// input drifts between attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
    /// The run this call belongs to.
    pub run_id: RunId,
    /// The journal sequence of the entry that opened this step.
    pub step_seq: Seq,
    /// `uuid5(run_id ‖ step_seq)` — the key every side effect must upsert on.
    pub effect_id: Uuid,
    /// `true` when a previous attempt at this step may already have run.
    ///
    /// The only field that varies between attempts. See
    /// [`ToolCall::is_replay`].
    pub replay: bool,
    /// When the step was **opened** — the `created_at` of its `step_started` or
    /// `approval_requested` entry.
    ///
    /// Deliberately not "when this attempt started": a re-execution after a
    /// crash sees exactly the value the first attempt saw, so a tool that stamps
    /// a row from it writes the same stamp twice rather than two different ones.
    /// A tool that needs the true current instant should read the clock itself
    /// and accept that doing so makes it non-idempotent.
    pub opened_at: DateTime<Utc>,
}
