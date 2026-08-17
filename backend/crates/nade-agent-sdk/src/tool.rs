//! The capability-facing trait, the tool set the engine dispatches through,
//! and the size cap every result passes on its way to the journal.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::message::{ToolCall, ToolSchema};

/// What the engine will do with a step whose outcome it cannot determine.
///
/// A step is ambiguous when its `step_started` entry committed but its
/// `step_done` did not: the process died somewhere between the fence and the
/// result, and nothing in the journal says whether the effect landed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPolicy {
    /// Execute the step again, with the same [`ToolCall::effect_id`].
    ///
    /// The default, and correct for **every effect that has a natural key**: the
    /// second attempt upserts onto the row the first one wrote, so two attempts
    /// collapse into one effect.
    #[default]
    Retry,
    /// Refuse to execute the step again.
    ///
    /// For effects that genuinely cannot be keyed — the canonical example is an
    /// SMTP `DATA` command that has already left the process. A blind retry
    /// might send a second copy; skipping might send none. Neither is a decision
    /// this crate is entitled to make on its own, so it makes neither: the run
    /// ends [`RunStatus::Failed`](crate::RunStatus::Failed) with
    /// [`FailureReason::AmbiguousEffect`](crate::FailureReason::AmbiguousEffect),
    /// quoting the `effect_id`, and the host reconciles against its own records.
    ///
    /// This is a **guard rail, not a solution**. An outbox keyed on `effect_id`
    /// — write the intent inside the same transaction as the effect row, deliver
    /// it once from a sweeper — turns the problem back into
    /// [`Retry`](ReplayPolicy::Retry) and needs no human at all.
    ///
    /// The choice is recorded on the entry that opens the step, so the refusal
    /// is as durable as the step itself. See
    /// [`Tool::replay_policy`](Tool::replay_policy).
    Halt,
}

/// Something the agent can do.
///
/// # Idempotency is the implementor's job
///
/// The engine guarantees that a step's `step_started` entry is durable *before*
/// [`Tool::execute`] is called, and that a step whose `step_done` never
/// committed is re-executed on the next run. It cannot guarantee a tool runs
/// only once — nothing can, across a process death. What it does guarantee is
/// that every attempt at the same step is handed the same
/// [`ToolCall::effect_id`]. Write your effects keyed on that id, with an
/// upsert, and a re-execution collapses onto the row the first attempt wrote.
///
/// **Approval does not change this.** A gated tool is re-executed after a crash
/// exactly like an ungated one; the human authorised the action once, and the
/// journal cannot tell a completed action from an interrupted one. See
/// [`ReplayPolicy`] for the only lever the engine offers, and the [crate
/// docs](crate#approval-is-not-idempotency) for why an outbox is the real answer.
///
/// # What a tool may rely on across attempts
///
/// Every attempt at one step is handed byte-identical input:
/// [`name`](ToolCall::name), [`arguments`](ToolCall::arguments),
/// [`id`](ToolCall::id), and a [`CallContext`](crate::CallContext) whose
/// `run_id`, `step_seq`, `effect_id` and
/// [`opened_at`](crate::CallContext::opened_at) are all fixed when the step is
/// opened and replayed verbatim afterwards.
///
/// [`ToolCall::is_replay`] is the **only** thing that differs between attempts,
/// and it is advisory: log it, skip an expensive re-read with it, but never let
/// it change the effect. A tool whose output depends on the attempt number, on
/// the wall clock, or on anything else outside the call has broken the
/// idempotency contract, and no amount of machinery in this crate can repair
/// that.
///
/// # Approval
///
/// [`Tool::requires_approval`] is consulted before a step is opened **and again
/// before every dispatch, including a re-dispatch after a crash**. It must
/// therefore be a pure function of the call's name and arguments: a decision
/// that flips between the two evaluations means the step was opened under
/// different rules than it would run under, and the engine refuses to execute it
/// with [`Error::ToolChanged`](crate::Error::ToolChanged) rather than guess. The
/// decision may depend on the arguments — "drafting a reply needs approval,
/// reading a thread does not" is a property of the call, not only of the tool.
///
/// The rule, in both directions: **a step never executes ungated if the tool
/// now requires approval, and a step opened gated must still hold a recorded
/// approval.** The second half does not consult this method at all — a step
/// opened behind a gate stays behind it for life, because the authority is the
/// human's recorded yes and not the current build's policy.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls this by. Must be unique within an engine.
    fn name(&self) -> &str;

    /// JSON Schema for the arguments object, as advertised to the model.
    fn schema(&self) -> Value;

    /// What this does, in the model's terms. Advertised alongside the schema.
    fn description(&self) -> Option<&str> {
        None
    }

    /// An opaque tag for *this implementation's behaviour*.
    ///
    /// Recorded with every step this tool opens, and compared before every
    /// re-dispatch. A step opened under one version is never executed under
    /// another: the [`effect_id`](crate::effect_id) was minted for a particular
    /// action, and silently running a different one under it is the failure the
    /// whole protocol exists to prevent.
    ///
    /// `None` is the default and is *not* "no checking" — the engine still
    /// fingerprints the name, description and schema, so a changed interface is
    /// caught for free. Override this when behaviour changes without the
    /// interface changing: a new recipient policy, a different downstream
    /// endpoint, a rewritten side effect.
    fn version(&self) -> Option<&str> {
        None
    }

    /// Whether this specific call needs a human to say yes first.
    ///
    /// Defaults to `false`. Mutating tools should override it. Must be pure in
    /// the call's name and arguments — see the [trait docs](Tool#approval).
    fn requires_approval(&self, _call: &ToolCall) -> bool {
        false
    }

    /// What the engine may do with an interrupted attempt at this call.
    ///
    /// Defaults to [`ReplayPolicy::Retry`], which is correct whenever the effect
    /// is keyed on [`ToolCall::effect_id`]. Return
    /// [`ReplayPolicy::Halt`](ReplayPolicy::Halt) only for an effect that truly
    /// cannot be keyed, and read that variant's docs first — it stops the run
    /// rather than fixing anything.
    ///
    /// # When it is asked, and which answer counts
    ///
    /// The answer given when the step **opens** is written to the journal and is
    /// the one that binds: a step opened under `Halt` is never blind-retried,
    /// however a later build answers. The method is asked again before a
    /// re-dispatch, but only so the decision can become *stricter* — a build
    /// that newly declares an effect unkeyable stops a step opened under
    /// `Retry`. It can never go the other way, because "this must not run twice"
    /// is a fact about an effect that may already exist, and re-asking a tool is
    /// not a way to unlearn it.
    fn replay_policy(&self, _call: &ToolCall) -> ReplayPolicy {
        ReplayPolicy::Retry
    }

    /// Do the thing.
    ///
    /// An [`Err`] is not fatal: the engine converts it into a structured tool
    /// result the model can read and recover from, and the run continues. A
    /// panic is caught and treated the same way.
    async fn execute(&self, call: &ToolCall) -> Result<Value>;
}

#[async_trait]
impl<T: Tool + ?Sized> Tool for Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn schema(&self) -> Value {
        (**self).schema()
    }

    fn description(&self) -> Option<&str> {
        (**self).description()
    }

    fn version(&self) -> Option<&str> {
        (**self).version()
    }

    fn requires_approval(&self, call: &ToolCall) -> bool {
        (**self).requires_approval(call)
    }

    fn replay_policy(&self, call: &ToolCall) -> ReplayPolicy {
        (**self).replay_policy(call)
    }

    async fn execute(&self, call: &ToolCall) -> Result<Value> {
        (**self).execute(call).await
    }
}

/// A stable fingerprint of a tool's advertised interface and declared version.
///
/// `sha256:<hex>` over the canonical JSON of `{name, description, version,
/// schema}`. Recorded on the entry that opens a step and checked before every
/// re-dispatch, so a step can never execute under an implementation other than
/// the one it was opened under. Object keys are emitted in sorted order (this
/// crate deliberately does not enable `serde_json/preserve_order`), so the value
/// is stable across processes and platforms.
pub fn tool_fingerprint(tool: &dyn Tool) -> String {
    crate::ids::args_hash(&json!({
        "name": tool.name(),
        "description": tool.description(),
        "version": tool.version(),
        "schema": tool.schema(),
    }))
}

/// The host-enforced allowlist: the only tools an engine will ever dispatch.
///
/// Ordered by name so the schema list handed to the model — and therefore the
/// prompt — is byte-stable across runs.
#[derive(Clone, Default)]
pub struct ToolSet {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolSet {
    /// Build a set, rejecting duplicate names.
    pub fn new(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Result<Self> {
        let mut map: BTreeMap<String, Arc<dyn Tool>> = BTreeMap::new();
        for tool in tools {
            let name = tool.name().to_string();
            if map.contains_key(&name) {
                return Err(Error::DuplicateTool(name));
            }
            map.insert(name, tool);
        }
        Ok(Self { tools: map })
    }

    /// Look a tool up by the name the model used.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Every name in the set, sorted.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// The schemas to advertise to the model, sorted by name.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().map(str::to_string),
                parameters: t.schema(),
            })
            .collect()
    }

    /// Number of tools in the set.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the set is empty. An empty set is legal: the model is simply
    /// told it has no tools.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl std::fmt::Debug for ToolSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSet")
            .field("names", &self.names())
            .finish()
    }
}

/// Out-of-band instructions a tool can return to the engine.
///
/// A tool that only produces values never touches this module. The one control
/// signal that exists parks the run on a timer, which is how the
/// `running → waiting(wake_at)` transition is reached without inventing a
/// second `execute` signature.
pub mod control {
    use chrono::{DateTime, Utc};
    use serde_json::{json, Value};

    /// The reserved key a control envelope hangs off.
    pub const CONTROL_KEY: &str = "__nade_control";

    /// What a control envelope asks the engine to do.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Control {
        /// Park the run until `until`, then continue where it left off.
        Wait {
            /// The earliest instant the host should wake the run.
            until: DateTime<Utc>,
        },
    }

    /// Park the run until `until`, recording `result` as the step's result.
    ///
    /// The run becomes [`RunOutcome::Waiting`](crate::RunOutcome::Waiting); the
    /// host is expected to schedule a wake-up and then call
    /// [`Engine::resume`](crate::Engine::resume) with
    /// [`Resolution::Timer`](crate::Resolution::Timer). `result` is what the
    /// model sees as the tool's answer once the run continues — the envelope
    /// itself is stripped and never reaches the model.
    ///
    /// ```
    /// use chrono::{Duration, Utc};
    /// use nade_agent_sdk::control;
    /// use serde_json::json;
    ///
    /// let parked = control::wait_until(Utc::now() + Duration::minutes(5), json!({"slept": true}));
    /// assert!(control::parse(&parked).is_some());
    /// ```
    pub fn wait_until(until: DateTime<Utc>, result: Value) -> Value {
        json!({
            CONTROL_KEY: { "kind": "wait", "until": until.to_rfc3339() },
            "result": result,
        })
    }

    /// Read a control envelope back, returning the instruction and the inner
    /// result. `None` for an ordinary tool result.
    pub fn parse(value: &Value) -> Option<(Control, Value)> {
        let envelope = value.get(CONTROL_KEY)?.as_object()?;
        match envelope.get("kind")?.as_str()? {
            "wait" => {
                let until = envelope.get("until")?.as_str()?;
                let until = DateTime::parse_from_rfc3339(until)
                    .ok()?
                    .with_timezone(&Utc);
                let inner = value.get("result").cloned().unwrap_or(Value::Null);
                Some((Control::Wait { until }, inner))
            }
            // EDGE: an unknown control kind is not a control envelope as far as
            // this version is concerned; the value flows through to the model
            // untouched rather than aborting the run.
            _ => None,
        }
    }
}

/// The key set on a result that had to be cut down to fit the size cap.
pub const TRUNCATED_KEY: &str = "nade_truncated";

/// Whether a result is the truncation envelope rather than a tool's own value.
pub fn is_truncated(value: &Value) -> bool {
    value
        .get(TRUNCATED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Roughly how many bytes the truncation envelope itself needs, excluding the
/// preview.
const ENVELOPE_OVERHEAD: usize = 256;

/// Cap a tool result at `limit` serialised bytes.
///
/// Returns the value to journal and hand to the model, plus whether anything
/// was cut. Oversized results are **replaced** by an explicit envelope rather
/// than silently shortened, so neither the model nor a human reading the run
/// log can mistake a fragment for the whole answer:
///
/// ```json
/// {
///   "nade_truncated": true,
///   "reason": "tool result exceeded the configured size cap",
///   "original_bytes": 10485760,
///   "limit_bytes": 16384,
///   "preview": "…the first N bytes of the serialised result…"
/// }
/// ```
///
/// The envelope serialises to no more than `limit` bytes whenever `limit` is at
/// least a few hundred: the preview budget is halved until it fits, so JSON
/// escaping cannot push it back over. Below that, the envelope's own fields are
/// an irreducible floor and the empty-preview envelope is returned as-is. The
/// preview is cut on a UTF-8 character boundary, never mid-code-point.
pub(crate) fn cap_result(value: Value, limit: usize) -> (Value, bool) {
    let encoded = match serde_json::to_vec(&value) {
        Ok(bytes) => bytes,
        // EDGE: a value that will not serialise (only reachable via a custom
        // Serialize impl upstream) is replaced rather than propagated, because
        // an unjournalable result would strand the run.
        Err(err) => {
            return (
                json!({
                    TRUNCATED_KEY: true,
                    "reason": "tool result could not be serialised",
                    "error": err.to_string(),
                }),
                true,
            );
        }
    };

    if encoded.len() <= limit {
        return (value, false);
    }

    let original_bytes = encoded.len();
    // `serde_json` always emits valid UTF-8, so this borrow never allocates.
    let text = String::from_utf8_lossy(&encoded);
    let mut budget = limit.saturating_sub(ENVELOPE_OVERHEAD);

    loop {
        let envelope = json!({
            TRUNCATED_KEY: true,
            "reason": "tool result exceeded the configured size cap",
            "original_bytes": original_bytes,
            "limit_bytes": limit,
            "preview": truncate_utf8(&text, budget),
        });
        let fits = serde_json::to_vec(&envelope)
            .map(|b| b.len() <= limit)
            .unwrap_or(false);
        if fits || budget == 0 {
            return (envelope, true);
        }
        budget /= 2;
    }
}

/// The longest prefix of `s` that is at most `max_bytes` long and ends on a
/// character boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = 0;
    for (idx, ch) in s.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &s[..end]
}

/// The structured error the model receives when it names a tool that does not
/// exist. The available names are included so it can correct itself.
pub(crate) fn unknown_tool_error(name: &str, available: &[String]) -> Value {
    json!({
        "error": {
            "code": "unknown_tool",
            "message": format!("no tool named '{name}' is available to this agent"),
            "tool": name,
            "available_tools": available,
        }
    })
}

/// The structured error the model receives when a tool returned `Err`.
pub(crate) fn tool_error(code: &str, name: &str, message: &str) -> Value {
    json!({
        "error": {
            "code": code,
            "message": message,
            "tool": name,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_utf8_never_splits_a_code_point() {
        // Four bytes each.
        let s = "😀😀😀";
        for budget in 0..=s.len() + 2 {
            let cut = truncate_utf8(s, budget);
            assert!(cut.len() <= budget.min(s.len()));
            assert!(s.starts_with(cut));
            // The only proof that matters: it is still valid UTF-8 and a prefix
            // of whole characters.
            assert_eq!(cut.chars().count(), cut.len() / 4);
        }
    }

    #[test]
    fn cap_result_leaves_small_values_alone() {
        let value = json!({"ok": true});
        let (capped, truncated) = cap_result(value.clone(), 1024);
        assert_eq!(capped, value);
        assert!(!truncated);
    }

    #[test]
    fn cap_result_envelope_always_fits_the_limit() {
        // A payload of quote characters is the worst case for JSON escaping:
        // every byte doubles when re-encoded into the preview string.
        for limit in [0usize, 1, 16, 64, 300, 1024, 4096] {
            let hostile = "\"".repeat(50_000);
            let (capped, truncated) = cap_result(json!({ "v": hostile }), limit);
            assert!(truncated, "limit {limit} should truncate");
            assert!(is_truncated(&capped));
            let encoded = serde_json::to_vec(&capped).expect("envelope serialises");
            if limit >= ENVELOPE_OVERHEAD {
                assert!(
                    encoded.len() <= limit,
                    "limit {limit} produced {} bytes",
                    encoded.len()
                );
            }
        }
    }
}
