//! Turning a sentence into a spec, at save time.
//!
//! `POST /agents` takes an `nl_definition` — "When a recruiter emails about a
//! tech role, save the next steps as a note." — and compiles it into the
//! machine-readable `spec` of `API.md` §5.1, plus the three fragments the
//! builder screen underlines.
//!
//! # Why this does not go through `Llm::chat`
//!
//! It needs two things the SDK's provider-neutral `ChatRequest` cannot express:
//! a **forced** `tool_choice`, so the model must answer with one structured
//! object rather than prose, and `strict: true`, so that object validates
//! against the schema before it ever reaches us. The compiler is not an agent
//! run — it has no journal, no steps and no approval — so going around the
//! engine is the honest layering, not a shortcut.
//!
//! # A failure here is not an HTTP failure
//!
//! `API.md` §5 is explicit: if compilation fails the agent is **still created**,
//! as a `draft`, with `spec: null` and `compile_error` set, "so the user's
//! sentence is never lost". Every error in this module is therefore a value the
//! caller stores, not a 5xx it returns.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::llm::{
    anthropic::{Client, WireMessage, WireRequest},
    cost::Tokens,
    ledger::{self, Record, SpendGuard},
    Purpose,
};

/// `API.md` §0 caps `nl_definition` at 4 000 characters.
pub const MAX_NL_DEFINITION: usize = 4_000;

/// What the compiler produces on success.
///
/// The three span fields exist because the builder renders
/// **"When {when_span}, {do_span}."** with both underlined. `validate.py`
/// enforces the invariant they participate in: with a spec, `when_span` and
/// `do_span` are never null; `trailing` may be, because not every sentence has
/// a closing clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub name: String,
    pub spec: Value,
    pub when_span: String,
    pub do_span: String,
    pub trailing: Option<String>,
    pub allowed_tools: Vec<String>,
}

/// Why a sentence did not compile. Stored in `agents.compile_error` verbatim,
/// so every variant has to read as an explanation to a person.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("This server has no AI model configured, so agents cannot be compiled yet.")]
    NotConfigured,
    /// The account is out of budget for today.
    ///
    /// Deliberately **not** stored in `agents.compile_error`: it is not a fact
    /// about the sentence, it is a fact about today, and a sentence that would
    /// compile tomorrow must not be recorded as one that cannot. The handlers
    /// map it to `429` and the client keeps the text.
    #[error("Your agents have used today's AI budget. Try again tomorrow.")]
    CeilingReached,
    #[error("The AI model could not be reached. The agent was saved; edit it to try again.")]
    Unavailable,
    #[error("The AI model's answer could not be understood: {0}")]
    Malformed(String),
    #[error("That sentence could not be turned into an agent: {0}")]
    Rejected(String),
    /// The caller sent nothing, or more than `API.md` §0 allows.
    ///
    /// Like [`Self::CeilingReached`] and unlike the rest, these are **not**
    /// stored in `agents.compile_error`: there is no agent to store them
    /// against. Both handlers map them to `400`.
    ///
    /// They live here rather than in the handlers because this is the one place
    /// every compile passes through, and the ceiling check below is here for
    /// exactly the same reason. A cap enforced only by the two callers that
    /// exist today is a cap the third one does not get.
    #[error("Describe what the agent should do.")]
    Empty,
    #[error("That description is too long; keep it under {MAX_NL_DEFINITION} characters.")]
    TooLong,
}

impl CompileError {
    /// Whether this is a fact about the *request* rather than about the
    /// sentence — so the handler answers `400` and stores nothing.
    #[must_use]
    pub const fn is_bad_request(&self) -> bool {
        matches!(self, Self::Empty | Self::TooLong)
    }
}

/// The tool the model is forced to call. Its schema *is* the output contract.
fn emit_tool_schema() -> Value {
    // `strict: true` requires `additionalProperties: false` and a complete
    // `required` list at every level, or the provider refuses the tool.
    json!({
        "name": "emit_agent",
        "description": "Return the compiled agent for the user's sentence.",
        "strict": true,
        "input_schema": {
            "type": "object",
            "additionalProperties": false,
            "required": [
                "name", "when_span", "do_span", "trailing",
                "trigger_kind", "instruction", "tools", "output_kind",
                "semantic", "from_domains", "subject_contains", "label_ids"
            ],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "A short title for this agent, at most 40 characters."
                },
                "when_span": {
                    "type": "string",
                    "description": "The exact substring of the user's sentence describing WHEN \
                                    the agent runs. Must appear in the sentence verbatim."
                },
                "do_span": {
                    "type": "string",
                    "description": "The exact substring describing WHAT it does. Must appear \
                                    in the sentence verbatim."
                },
                "trailing": {
                    "type": ["string", "null"],
                    "description": "Any trailing clause such as 'Ask me before you save.', \
                                    or null if there is none."
                },
                "trigger_kind": {
                    "type": "string",
                    "enum": ["mail", "manual"],
                    "description": "'mail' if new email should trigger it, otherwise 'manual'."
                },
                "semantic": {
                    "type": ["string", "null"],
                    "description": "A sentence describing which messages match, checked by a \
                                    model after the filters below. Null if the filters suffice."
                },
                "from_domains": {
                    "type": "array", "items": {"type": "string"},
                    "description": "Sender domains to match, or an empty array."
                },
                "subject_contains": {
                    "type": "array", "items": {"type": "string"},
                    "description": "Substrings the subject must contain, or an empty array."
                },
                "label_ids": {
                    "type": "array", "items": {"type": "string"},
                    "description": "Gmail label ids to restrict to. Use [\"INBOX\"] by default."
                },
                "instruction": {
                    "type": "string",
                    "description": "The instruction given to the agent on every run."
                },
                "tools": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "enum": ["search_mail", "read_thread", "write_note", "draft_reply"]
                    },
                    "description": "The tools this agent needs. Prefer the smallest set."
                },
                "output_kind": {
                    "type": "string", "enum": ["note", "draft", "none"],
                    "description": "What the agent produces."
                }
            }
        }
    })
}

const SYSTEM: &str = "\
You compile a person's one-sentence description of an email agent into a structured \
definition. You always answer by calling `emit_agent` exactly once.

Rules:
- `when_span` and `do_span` must be copied VERBATIM from the user's sentence. Do not \
  paraphrase them, do not fix their grammar, and do not include the leading word 'When' \
  or the trailing full stop. The app underlines these two spans inside the sentence the \
  person typed, so a span that does not appear in it character for character is unusable.
- Many sentences carry no explicit 'when' clause - 'Search my mail for interview emails \
  and save what I need to do' is an instruction, not a trigger. Do NOT invent one. Quote \
  the smallest phrase already present that says WHICH emails the agent cares about; here \
  that is 'interview emails'. If the sentence names no subject at all, quote its first \
  few words rather than writing something new.
- Choose the smallest set of tools that can do the job. An agent that only reads and \
  takes notes needs `read_thread` and `write_note`; it does not need `draft_reply`.
- The agent can never send, forward or delete anything. It can only read mail, search, \
  save notes and prepare drafts for the person to review.
- Prefer deterministic filters (`from_domains`, `subject_contains`) over `semantic`. Use \
  `semantic` only for a judgement a filter cannot express.";

/// Compile `nl_definition`, and record what the call cost.
///
/// # Errors
/// Every variant of [`CompileError`] is a value the caller stores in
/// `agents.compile_error`; none of them is an HTTP failure.
pub async fn compile(
    client: Option<&Arc<Client>>,
    pool: &PgPool,
    guard: &SpendGuard,
    nl_definition: &str,
) -> Result<Compiled, CompileError> {
    // Before the provider is even looked up: an unbounded user string must not
    // reach a model, and an empty one has nothing to compile. Counted in
    // characters, as `API.md` §0 states the cap.
    //
    // EDGE (empty input, unicode): `trim` catches whitespace-only, and
    // `chars().count()` is the contract's unit — `len()` would reject a
    // 1 400-character sentence written in Japanese.
    let nl_definition = nl_definition.trim();
    if nl_definition.is_empty() {
        return Err(CompileError::Empty);
    }
    if nl_definition.chars().count() > MAX_NL_DEFINITION {
        return Err(CompileError::TooLong);
    }

    let Some(client) = client else {
        return Err(CompileError::NotConfigured);
    };
    let account_id = guard.account_id();

    // The ceiling lives **here**, not in the two handlers, because this is the
    // one place both of them pass through and a third caller would otherwise
    // inherit nothing. `POST /agents` and `PATCH /agents/{id}` each reach a
    // model, each costs real money, and neither is a run - so `SpendGuard`,
    // which the run path gets through the adapter, has to be applied by hand on
    // this one. Without it the daily ceiling bounds one of the three paths that
    // spend, and an authenticated caller can loop the other two without limit.
    match guard.check().await {
        Ok(Ok(())) => {}
        Ok(Err(_breached)) => return Err(CompileError::CeilingReached),
        Err(err) => {
            tracing::error!(error = %err, "could not read the spend ledger before compiling");
            return Err(CompileError::Unavailable);
        }
    }

    let model = client.compile_model().to_owned();

    let request = WireRequest {
        model: model.clone(),
        max_tokens: 2_048,
        system: Some(SYSTEM.to_owned()),
        messages: vec![WireMessage {
            role: "user",
            content: vec![json!({"type": "text", "text": nl_definition})],
        }],
        tools: vec![emit_tool_schema()],
        // Forced: the model must call the tool, so the answer is an object and
        // never prose that has to be scraped.
        tool_choice: Some(json!({"type": "tool", "name": "emit_agent"})),
        temperature: None,
    };

    let response = match client.send(&request).await {
        Ok(response) => response,
        Err(err) => {
            record_call(
                pool,
                account_id,
                &model,
                Tokens::default(),
                Some(&err.to_string()),
            )
            .await;
            return Err(match err {
                crate::llm::anthropic::Error::NotConfigured => CompileError::NotConfigured,
                crate::llm::anthropic::Error::Permanent { message, .. } => {
                    CompileError::Malformed(message)
                }
                _ => CompileError::Unavailable,
            });
        }
    };

    // Priced and recorded **before** the answer is parsed. The provider
    // answered 200, so the tokens were billed; an early return here - a body
    // that will not decode, or a model that answered without calling the forced
    // tool - would spend money the ledger never sees, and the ceiling
    // under-counts by exactly the calls that went wrong. The adapter closes the
    // same hole on the run path.
    let tokens = response.usage();
    let reported = response.model_name().to_owned();
    let billed = if reported.is_empty() { model } else { reported };
    record_call(pool, account_id, &billed, tokens, None).await;

    let call = extract_call(response)?;
    build(nl_definition, &call)
}

/// Pull the forced tool call's input out of a raw response.
fn extract_call(response: crate::llm::anthropic::WireResponse) -> Result<Value, CompileError> {
    let (chat, _tokens, _model) = crate::llm::anthropic::from_wire(response)
        .map_err(|e| CompileError::Malformed(e.to_string()))?;

    let call = chat
        .tool_calls
        .into_iter()
        .find(|call| call.name == "emit_agent")
        .ok_or_else(|| {
            CompileError::Malformed("the model answered without calling `emit_agent`".to_owned())
        })?;
    Ok(call.arguments)
}

/// Write one ledger row, loudly on failure.
async fn record_call(
    pool: &PgPool,
    account_id: Uuid,
    model: &str,
    tokens: Tokens,
    error: Option<&str>,
) {
    let record = match error {
        None => Record::ok(account_id, Purpose::Compile, model, tokens),
        Some(message) => Record::failed(account_id, Purpose::Compile, model, tokens, message),
    };
    ledger::record_or_log(pool, &record).await;
}

/// Validate the model's object and assemble the stored shape.
///
/// Everything the model says is checked here rather than trusted: `API.md`
/// §5.1 is the contract, `docs/contract/validate.py` enforces the same rules on
/// the fixtures, and a spec that violates them would be stored and then served.
fn build(nl_definition: &str, call: &Value) -> Result<Compiled, CompileError> {
    let object = call.as_object().ok_or_else(|| {
        CompileError::Malformed("`emit_agent` was not called with an object".into())
    })?;

    let name = trimmed(object, "name")?;
    let name = crate::agents::tools::cap(&name, 80);

    // The two spans must be *verbatim* substrings, because the builder screen
    // underlines them inside the sentence it renders. A paraphrase would
    // underline nothing, and the screen would silently look broken.
    let when_span = verbatim_span(object, "when_span", nl_definition)?;
    let do_span = verbatim_span(object, "do_span", nl_definition)?;
    let trailing = object
        .get("trailing")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let trigger_kind = trimmed(object, "trigger_kind")?;
    // P4 compiles `mail` and `manual` only. `schedule` is refused because
    // `validate.py` requires `trigger.kind == "schedule"` to imply a non-null
    // `agents.schedule`, and deriving one is P7's job - storing a schedule
    // trigger with no schedule would be a contract violation on every read.
    if !matches!(trigger_kind.as_str(), "mail" | "manual") {
        return Err(CompileError::Rejected(format!(
            "trigger kind {trigger_kind:?} is not supported yet; scheduled agents arrive later"
        )));
    }

    let mut tools = string_array(object, "tools");
    tools.retain(|tool| crate::agents::tools::V1_TOOLS.contains(&tool.as_str()));
    tools.sort();
    tools.dedup();
    if tools.is_empty() {
        return Err(CompileError::Rejected(
            "no usable tools were chosen for this agent".to_owned(),
        ));
    }

    let output_kind = trimmed(object, "output_kind").unwrap_or_else(|_| "none".to_owned());
    if !matches!(output_kind.as_str(), "note" | "draft" | "none") {
        return Err(CompileError::Rejected(format!(
            "output kind {output_kind:?} is not one this version can produce"
        )));
    }

    let instruction = trimmed(object, "instruction")?;

    let mut label_ids = string_array(object, "label_ids");
    if label_ids.is_empty() {
        label_ids.push("INBOX".to_owned());
    }

    let spec = json!({
        "version": 1,
        "trigger": {
            "kind": trigger_kind,
            "filters": {
                "from_domains": string_array(object, "from_domains"),
                "from_contains": [],
                "subject_contains": string_array(object, "subject_contains"),
                "body_contains": [],
                "label_ids": label_ids,
                "has_attachment": Value::Null,
                // PLAN.md's dev cap, applied to every compiled agent rather
                // than left to the model to remember.
                "newer_than_days": 30,
            },
            "semantic": object
                .get("semantic")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or(Value::Null, |value| Value::String(value.to_owned())),
        },
        "instruction": instruction,
        "tools": tools.clone(),
        "output": {
            "kind": output_kind,
            "title_template": Value::Null,
        },
    });

    Ok(Compiled {
        name,
        spec,
        when_span,
        do_span,
        trailing,
        // `spec.tools` must be a subset of `allowed_tools`, and the host
        // enforces `allowed_tools` at dispatch whatever the spec says. Equal at
        // creation is the only sane starting point; `PATCH` can widen or
        // narrow it afterwards.
        allowed_tools: tools,
    })
}

fn trimmed(object: &Map<String, Value>, key: &str) -> Result<String, CompileError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| CompileError::Malformed(format!("`{key}` was missing or empty")))
}

/// The model's arrays, additionally trimmed and stripped of empties — a model
/// that emits `["read_thread", " "]` should not produce a tool named `" "`.
fn string_array(object: &Map<String, Value>, key: &str) -> Vec<String> {
    crate::json::str_array(object.get(key))
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A span the builder can underline: present in the sentence, character for
/// character.
fn verbatim_span(
    object: &Map<String, Value>,
    key: &str,
    nl_definition: &str,
) -> Result<String, CompileError> {
    let span = trimmed(object, key)?;
    if nl_definition.contains(&span) {
        return Ok(span);
    }
    // Case-insensitive rescue before giving up: models routinely capitalise the
    // first word of a clause they quote, and the sentence is still the one the
    // user wrote. Anything further from the original is a paraphrase, and a
    // paraphrase cannot be underlined.
    find_case_insensitive(nl_definition, &span)
        .map(|(start, end)| nl_definition[start..end].to_owned())
        .ok_or_else(|| {
            CompileError::Rejected(format!(
                "the model paraphrased the sentence instead of quoting it ({key})"
            ))
        })
}

/// Find `needle` in `haystack`, ignoring case, and return **byte offsets into
/// `haystack`**.
///
/// The obvious spelling of this — `haystack.to_lowercase().find(...)` and then
/// slicing `haystack` with the result — is unsound, and it panics on real
/// input. `str::to_lowercase` is full Unicode case mapping and does not
/// preserve byte length: `İ` (U+0130, 2 bytes) lowercases to `i` + U+0307
/// (3 bytes), and `ẞ` (3 bytes) to `ß` (2). So an index into the lowercased
/// string is not an index into the original, and adding the *needle's* length
/// to it is a third unrelated number. Measured, on this exact code:
///
/// * `İx` + span `X` → byte index 4 in a 3-byte string: **panic**;
/// * `İÉ save this` + span `é save` → start 3 is not a char boundary: **panic**;
/// * `ẞIG news…` + span `ßig news` → silently returns `ẞIG new`, one character
///   short, which the builder then underlines wrongly with no error anywhere.
///
/// The panic reaches `POST /agents` from a 4 000-character user string, becomes
/// a 500 through `CatchPanicLayer`, and loses the sentence — the one outcome
/// `API.md` §5 says must never happen.
///
/// So this walks characters and compares them case-folded, never mixing an
/// offset from one string with a length from another.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    for (start, _) in haystack.char_indices() {
        let mut hay = haystack[start..].chars();
        let mut end = start;
        let mut matched = true;
        for want in needle.chars() {
            match hay.next() {
                Some(got) if got.to_lowercase().eq(want.to_lowercase()) => {
                    end += got.len_utf8();
                }
                _ => {
                    matched = false;
                    break;
                }
            }
        }
        if matched {
            return Some((start, end));
        }
    }
    None
}

#[cfg(test)]
mod tests;
