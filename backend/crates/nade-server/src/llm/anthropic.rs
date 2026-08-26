//! The Anthropic Messages adapter.
//!
//! Two layers, deliberately:
//!
//! * [`Client`] is the HTTP half — one `POST /v1/messages`, the retry policy,
//!   and the wire types. It is used directly by the spec compiler, which needs
//!   a forced `tool_choice` and `strict` tools that the SDK's provider-neutral
//!   [`ChatRequest`] cannot express.
//! * [`Adapter`] implements [`Llm`] over it, and is what an agent run uses.
//!
//! # The translation is not a rename
//!
//! The SDK's conversation model and Anthropic's differ in five ways that each
//! produce a `400` (or worse, a silently degraded conversation) if missed. They
//! are enumerated on [`to_wire`], and each has a test.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use durable_agent::{
    ChatRequest, ChatResponse, Error as SdkError, Llm, Message, Result as SdkResult, StopReason,
    TokenUsage, ToolCall,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    cost::Tokens,
    ledger::{self, Record, SpendGuard},
    Purpose, API_VERSION,
};
use crate::{config::LlmConfig, gmail::quota};

/// Sent when neither the caller nor the config names a ceiling.
///
/// Anthropic requires `max_tokens`; the SDK's `max_output_tokens` is an
/// `Option`. 4096 is comfortably above anything a v1 tool call or a compiled
/// spec needs, and well under the per-run token budget.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4_096;

// ---------------------------------------------------------------- errors --

/// What went wrong talking to the provider.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server is not configured with a key. Not a failure of the call —
    /// a failure to have been set up — so it is worth its own variant.
    #[error("no ANTHROPIC_API_KEY is configured; agent features are unavailable")]
    NotConfigured,
    /// A status the provider will never answer differently: a malformed
    /// request, a bad key, an unknown model. Retrying spends time and money to
    /// get the same answer.
    #[error("the model provider rejected the request ({status}): {message}")]
    Permanent { status: u16, message: String },
    /// Retryable, and retried, but the attempts ran out.
    #[error("the model provider was unavailable after {attempts} attempts: {message}")]
    Unavailable { attempts: u32, message: String },
    /// The request could not be built from the conversation. A bug or a corrupt
    /// journal, never a provider problem.
    #[error("could not translate the conversation for the provider: {0}")]
    Translation(String),
    /// The answer did not parse.
    #[error("the model provider's response could not be parsed: {0}")]
    Malformed(String),
}

impl Error {
    fn into_sdk(self) -> SdkError {
        SdkError::llm(self.to_string())
    }
}

// ------------------------------------------------------------ wire types --

#[derive(Debug, Serialize)]
pub struct WireRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct WireResponse {
    #[serde(default)]
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Debug, Default, Deserialize)]
pub struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

impl WireResponse {
    /// The model the provider says answered.
    ///
    /// An alias can resolve to a dated snapshot, so the ledger records this
    /// rather than the id we asked with.
    #[must_use]
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// The four token counters, without decoding the content.
    ///
    /// Exists so a caller can **price a response before parsing it**. A body
    /// that will not decode was still billed, and a ledger that only records
    /// the answers it understood under-counts by exactly the calls that went
    /// wrong.
    #[must_use]
    pub fn usage(&self) -> Tokens {
        Tokens {
            input: self.usage.input_tokens,
            output: self.usage.output_tokens,
            cache_write: self.usage.cache_creation_input_tokens,
            cache_read: self.usage.cache_read_input_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireError {
    #[serde(default)]
    error: WireErrorBody,
}

#[derive(Debug, Default, Deserialize)]
struct WireErrorBody {
    #[serde(default)]
    message: String,
}

// ----------------------------------------------------------- translation --

/// Turn an SDK [`ChatRequest`] into an Anthropic request body.
///
/// The five differences that matter, each of which is a `400` or a silent
/// degradation if it is skipped:
///
/// 1. **System is hoisted.** The SDK carries it as a `Message::System` inside
///    `messages` and prepends it to every turn; Anthropic takes a top-level
///    `system` field and rejects `system` as a message role.
/// 2. **After hoisting, the first message must be `user`.** Anthropic rejects a
///    conversation that opens on an assistant turn.
/// 3. **Consecutive tool results coalesce into one `user` message.** The SDK
///    emits one `Message::Tool` per call. Sending one message each either fails
///    role alternation or — worse, because it succeeds — teaches the model that
///    parallel tool calls get split up, and it stops making them.
/// 4. **`tool_result.content` must be a string.** The SDK's `result` is an
///    arbitrary `Value`.
/// 5. **Empty text blocks are rejected.** `text: None` is obvious; `Some("")`
///    is the one that slips through.
///
/// # Errors
/// Returns [`Error::Translation`] if the conversation cannot be represented —
/// an empty conversation, one that opens on an assistant turn, or a tool call
/// whose arguments are not a JSON object. All three mean the journal disagrees
/// with what any provider could have produced, and mangling it quietly would
/// send the model a different call from the one the journal records.
pub fn to_wire(
    req: &ChatRequest,
    fallback_model: &str,
    fallback_max_tokens: u32,
) -> Result<WireRequest, Error> {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<WireMessage> = Vec::new();

    for message in &req.messages {
        match message {
            // (1) hoisted, in order, wherever it appears.
            Message::System { text } => {
                if !text.trim().is_empty() {
                    system_parts.push(text.clone());
                }
            }
            Message::User { text } => {
                messages.push(WireMessage {
                    role: "user",
                    content: vec![json!({"type": "text", "text": text})],
                });
            }
            Message::Assistant { text, tool_calls } => {
                let mut content: Vec<Value> = Vec::new();
                // (5) drop empty text, both spellings.
                if let Some(text) = text.as_deref().filter(|t| !t.is_empty()) {
                    content.push(json!({"type": "text", "text": text}));
                }
                for call in tool_calls {
                    content.push(tool_use_block(call)?);
                }
                // EDGE: an assistant turn with neither prose nor calls has no
                // legal representation (Anthropic rejects empty content). The
                // engine never records one, and dropping it is safe precisely
                // because it had no tool calls, so nothing later refers to it.
                if content.is_empty() {
                    continue;
                }
                messages.push(WireMessage {
                    role: "assistant",
                    content,
                });
            }
            Message::Tool {
                call_id,
                result,
                is_error,
                ..
            } => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    // (4) always a string.
                    "content": stringify_result(result),
                    "is_error": is_error,
                });
                // (3) append to the previous user message when it is already a
                // run of tool results, rather than starting a new one.
                match messages.last_mut() {
                    Some(last) if last.role == "user" && is_all_tool_results(last) => {
                        last.content.push(block);
                    }
                    _ => messages.push(WireMessage {
                        role: "user",
                        content: vec![block],
                    }),
                }
            }
        }
    }

    // EDGE: empty input. A request with no messages at all is not something the
    // provider can answer, and its error would be far from the cause.
    if messages.is_empty() {
        return Err(Error::Translation(
            "the conversation has no messages the provider can be sent".to_owned(),
        ));
    }
    // (2) must open on a user turn.
    if messages[0].role != "user" {
        return Err(Error::Translation(format!(
            "the conversation opens on a {} turn; the provider requires a user turn first",
            messages[0].role
        )));
    }

    Ok(WireRequest {
        model: req
            .model
            .clone()
            .unwrap_or_else(|| fallback_model.to_owned()),
        // (6) `max_tokens` is mandatory on the wire and optional in the SDK.
        max_tokens: req.max_output_tokens.unwrap_or(fallback_max_tokens),
        system: (!system_parts.is_empty()).then(|| system_parts.join("\n\n")),
        messages,
        tools: req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description.clone().unwrap_or_default(),
                    "input_schema": tool.parameters,
                })
            })
            .collect(),
        tool_choice: None,
        temperature: req.temperature,
    })
}

fn tool_use_block(call: &ToolCall) -> Result<Value, Error> {
    // Anthropic requires `input` to be an object. The SDK deliberately does not
    // constrain `arguments` — "a model that emits a scalar or an array is
    // passed through unchanged so the tool can reject it" — so a non-object can
    // reach here from a foreign or hand-built journal. Refusing is the only
    // honest option: substituting `{}` would replay a *different* call than the
    // journal records, which is the exact failure the journal exists to prevent.
    if !call.arguments.is_object() {
        return Err(Error::Translation(format!(
            "tool call {} for {:?} has non-object arguments ({}), which the provider cannot \
             represent",
            call.id,
            call.name,
            kind_of(&call.arguments),
        )));
    }
    Ok(json!({
        "type": "tool_use",
        "id": call.id,
        "name": call.name,
        "input": call.arguments,
    }))
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A tool result as a string the model can read.
///
/// A JSON string is unwrapped rather than re-quoted: a tool that returns
/// `"ok"` should show the model `ok`, not `"ok"`. Everything else is compact
/// JSON.
fn stringify_result(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn is_all_tool_results(message: &WireMessage) -> bool {
    !message.content.is_empty()
        && message
            .content
            .iter()
            .all(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
}

/// Map Anthropic's `stop_reason` onto the SDK's enum.
///
/// The four the SDK names map across; everything else — `refusal`,
/// `pause_turn`, and whatever ships next — becomes [`StopReason::Other`] rather
/// than being guessed at or dropped.
fn stop_reason_from(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") | None => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some(other) => StopReason::Other(other.to_owned()),
    }
}

/// Pull the SDK's view of one turn out of a parsed response.
///
/// Public because the spec compiler goes around `Llm::chat` — it needs a forced
/// `tool_choice`, which the provider-neutral `ChatRequest` cannot express — and
/// still has to read the answer the same way an agent run does.
pub fn from_wire(response: WireResponse) -> Result<(ChatResponse, Tokens, String), Error> {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for block in &response.content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
            Some("tool_use") => {
                let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name.is_empty() {
                    return Err(Error::Malformed(
                        "a tool_use block carried no tool name".to_owned(),
                    ));
                }
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                tool_calls.push(ToolCall::new(id, name, input));
            }
            // Thinking blocks and anything the provider adds later are ignored
            // rather than fatal: a new block type must not break every run.
            _ => {}
        }
    }

    let tokens = response.usage();

    // The SDK budgets on two counters. Cache reads and writes are input tokens
    // the model actually processed, so they belong in `input_tokens` for the
    // budget's purposes even though they are billed at their own rates - which
    // is why the full breakdown goes to the ledger separately. At P4 caching is
    // off and both are zero.
    let usage = TokenUsage {
        input_tokens: tokens
            .input
            .saturating_add(tokens.cache_read)
            .saturating_add(tokens.cache_write),
        output_tokens: tokens.output,
    };

    let chat = ChatResponse {
        text: (!text.trim().is_empty()).then_some(text),
        tool_calls,
        stop_reason: stop_reason_from(response.stop_reason.as_deref()),
        usage,
    };
    Ok((chat, tokens, response.model))
}

// ---------------------------------------------------------------- client --

/// The HTTP half: one endpoint, one retry policy.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    api_key: String,
    api_base: String,
    timeout: Duration,
    max_attempts: u32,
    default_model: String,
    compile_model: String,
}

impl std::fmt::Debug for Client {
    /// Hand-written: a derived one would print the API key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("api_base", &self.api_base)
            .field("default_model", &self.default_model)
            .field("compile_model", &self.compile_model)
            .field("timeout", &self.timeout)
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Build a client, or explain why there is not one.
    ///
    /// # Errors
    /// [`Error::NotConfigured`] when no API key is set — which is survivable by
    /// design: the server boots without one and only agent routes fail.
    ///
    /// # Panics
    /// Never. The `expect` below is unreachable: `gmail::http_client` only fails
    /// if TLS cannot be initialised, which every other request path would have
    /// hit first.
    pub fn new(cfg: &LlmConfig) -> Result<Self, Error> {
        let api_key = cfg.api_key.clone().ok_or(Error::NotConfigured)?;
        guard_against_live_calls_in_tests(&cfg.api_base);
        // `gmail::http_client()` and not `reqwest::Client::builder()`:
        // `gmail::tests::no_bare_reqwest_clients` greps all of `src/` for the
        // latter. The name is Gmail's for historical reasons; the function is a
        // plain hardened client with the ring provider installed.
        let http = crate::gmail::http_client()
            .map_err(|e| Error::Translation(format!("could not build an HTTP client: {e}")))?;
        Ok(Self {
            http,
            api_key,
            api_base: cfg.api_base.trim_end_matches('/').to_owned(),
            timeout: cfg.timeout,
            max_attempts: cfg.max_attempts.max(1),
            default_model: cfg.model.clone(),
            compile_model: cfg.compile_model.clone(),
        })
    }

    /// The model an agent run uses unless its agent overrides it.
    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// The model `POST /agents` compiles with.
    #[must_use]
    pub fn compile_model(&self) -> &str {
        &self.compile_model
    }

    /// Send one request, retrying the statuses that are worth retrying.
    ///
    /// # Errors
    /// [`Error::Permanent`] for a status the provider will answer the same way
    /// forever, [`Error::Unavailable`] when the retries ran out,
    /// [`Error::Malformed`] when the body did not parse.
    pub async fn send(&self, request: &WireRequest) -> Result<WireResponse, Error> {
        let url = format!("{}/v1/messages", self.api_base);
        let mut last = String::from("no attempt was made");

        for attempt in 1..=self.max_attempts {
            // A per-request timeout, because `gmail::http_client()` bakes a
            // 60 s client-wide one: without this, NADE_LLM_TIMEOUT_SECS above
            // 60 would silently do nothing at all.
            let sent = self
                .http
                .post(&url)
                .timeout(self.timeout)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(request)
                .send()
                .await;

            let response = match sent {
                Ok(response) => response,
                Err(err) => {
                    // A connection failure or a timeout: retryable.
                    last = err.to_string();
                    self.backoff(attempt, None).await;
                    continue;
                }
            };

            let status = response.status();
            if status.is_success() {
                let body = response
                    .text()
                    .await
                    .map_err(|e| Error::Malformed(e.to_string()))?;
                return serde_json::from_str::<WireResponse>(&body)
                    .map_err(|e| Error::Malformed(format!("{e}: {}", preview(&body))));
            }

            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(quota::parse_retry_after);
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<WireError>(&body)
                .map(|e| e.error.message)
                .ok()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| preview(&body));

            if !is_retryable(status.as_u16()) {
                // 400, 401, 403, 404, 413: the answer will not change. Retrying
                // burns the job's attempts on a certainty.
                return Err(Error::Permanent {
                    status: status.as_u16(),
                    message,
                });
            }
            last = message;
            self.backoff(attempt, retry_after).await;
        }

        Err(Error::Unavailable {
            attempts: self.max_attempts,
            message: last,
        })
    }

    /// Sleep before the next attempt, unless this was the last one.
    async fn backoff(&self, attempt: u32, retry_after: Option<Duration>) {
        if attempt >= self.max_attempts {
            return;
        }
        // `gmail::quota::backoff` and not a second schedule. It already does
        // exponential-with-jitter, already prefers a server-sent `Retry-After`
        // when that is longer, and already caps both - and the version written
        // here was worse at two of the three. Its "jitter" was `attempt * 37 %
        // 250`, a pure function of the attempt number, so two workers that hit
        // the same 429 slept for *exactly* the same time: the one thing jitter
        // exists to prevent. And its `.min(30s)` was applied after the server's
        // figure, so a provider asking for two minutes got thirty seconds and
        // was hammered. `rand` is already in the tree, and the adapter already
        // reaches into `gmail::` for `http_client`.
        tokio::time::sleep(quota::backoff(attempt, retry_after, quota::random_jitter())).await;
    }
}

/// `429`, `500`-class and `529` are worth another go. Nothing else is.
const fn is_retryable(status: u16) -> bool {
    status == 429 || status == 529 || status >= 500
}

fn preview(body: &str) -> String {
    let cleaned = body.trim();
    match cleaned.char_indices().nth(200) {
        None => cleaned.to_owned(),
        Some((idx, _)) => format!("{}...", &cleaned[..idx]),
    }
}

/// Refuse to build a client aimed at the real API from inside the test suite.
///
/// `backend/justfile` sets `dotenv-load := true` and `backend/.env` holds a
/// live key, so a test that forgot to point at its `MockServer` would otherwise
/// send real requests and bill real money — silently, and only on the machine
/// that happens to have a key. `NADE_LIVE=1` opts a deliberate live smoke test
/// back in.
fn guard_against_live_calls_in_tests(api_base: &str) {
    if cfg!(test)
        && api_base.trim_end_matches('/') == super::DEFAULT_API_BASE
        && std::env::var("NADE_LIVE").as_deref() != Ok("1")
    {
        panic!(
            "a test built an Anthropic client pointed at the real API. Point it at a MockServer \
             with TestApp::set_llm_base, or set NADE_LIVE=1 if the live call is the point."
        );
    }
}

// ---------------------------------------------------- forced tool calls --

/// How one priced, forced tool call ended.
///
/// Every variant past `Called` has **already written its ledger row**, which is
/// the whole point of the type: the caller's job is to map an outcome to its own
/// error, not to remember what still has to be paid for.
#[derive(Debug)]
pub enum ForcedCall {
    /// The model called the tool. Its `input`, unvalidated.
    Called(Value),
    /// The daily ceiling stopped the request before it was made. Nothing was
    /// spent, so nothing is recorded.
    CeilingReached,
    /// The ledger could not be read, so the ceiling could not be honoured. The
    /// call is refused rather than made unpriced.
    LedgerUnavailable,
    /// The request failed. Recorded at zero tokens with the error.
    Failed(Error),
    /// A 200 that could not be read, or that did not call the forced tool.
    /// Priced and recorded first.
    Unreadable(String),
}

/// Check the ceiling, make one forced tool call, price it, and read the answer.
///
/// # Why this is a function and not a comment
///
/// Two callers want an *object* out of the model rather than prose — the spec
/// compiler and the mail trigger's semantic judgement — so neither can go
/// through [`Adapter`], whose [`ChatRequest`] cannot express a forced
/// `tool_choice` over a `strict` schema. Both therefore re-implemented the two
/// things the adapter does around a call, and **both defects the ledger has
/// ever had were a missing step of this exact chain**:
///
/// * **D60** — the ceiling belongs at the one place a path spends. A caller
///   that forgets it can be looped without limit by an authenticated user.
/// * **D61** — the row is written from [`WireResponse::usage`] **before** the
///   body is parsed. The provider answered 200, so the tokens were billed; an
///   early return on an unreadable body spends money the ledger never sees, and
///   the ceiling then under-counts by exactly the calls that went wrong.
///
/// A third copy was written at P5. This is the one, and it is right here beside
/// `Adapter::chat`, which does the same five steps for the other shape.
///
/// The caller keeps what genuinely differs: the request it builds, the tool it
/// forces, and how it maps [`ForcedCall`] onto its own errors.
pub async fn forced_tool_call(
    client: &Client,
    pool: &PgPool,
    guard: &SpendGuard,
    purpose: Purpose,
    agent_id: Option<Uuid>,
    request: &WireRequest,
    tool_name: &str,
) -> ForcedCall {
    let account_id = guard.account_id();
    let write = |model: String, tokens: Tokens, error: Option<String>| async move {
        let mut record = match error {
            None => Record::ok(account_id, purpose, &model, tokens),
            Some(message) => Record::failed(account_id, purpose, &model, tokens, &message),
        };
        record.agent_id = agent_id;
        ledger::record_or_log(pool, &record).await;
    };

    // D60, first and unconditionally.
    match guard.check().await {
        Ok(Ok(())) => {}
        Ok(Err(_breached)) => return ForcedCall::CeilingReached,
        Err(error) => {
            tracing::error!(%error, ?purpose, "could not read the spend ledger before a call");
            return ForcedCall::LedgerUnavailable;
        }
    }

    let asked_for = request.model.clone();
    let response = match client.send(request).await {
        Ok(response) => response,
        Err(error) => {
            write(asked_for, Tokens::default(), Some(error.to_string())).await;
            return ForcedCall::Failed(error);
        }
    };

    // D61. `model_name` is what the provider says it actually ran, which is not
    // always what was asked for, and an empty one falls back rather than
    // pricing against "".
    let tokens = response.usage();
    let reported = response.model_name().to_owned();
    let billed = if reported.is_empty() {
        asked_for
    } else {
        reported
    };
    write(billed, tokens, None).await;

    let (chat, _tokens, _model) = match from_wire(response) {
        Ok(parsed) => parsed,
        Err(error) => return ForcedCall::Unreadable(error.to_string()),
    };
    match chat
        .tool_calls
        .into_iter()
        .find(|call| call.name == tool_name)
    {
        Some(call) => ForcedCall::Called(call.arguments),
        None => ForcedCall::Unreadable(format!("the model answered without calling `{tool_name}`")),
    }
}

// --------------------------------------------------------------- adapter --

/// The [`Llm`] an agent run is built with: a [`Client`], plus everything the
/// ledger needs to attribute a call.
#[derive(Debug, Clone)]
pub struct Adapter {
    /// Set when the provider refused in a way no retry can fix.
    ///
    /// `Llm::chat` can only answer `Err`, and the host reads every `Err` as
    /// "retry this job later" — so a bad API key or a retired model id would
    /// otherwise burn five job attempts with exponential backoff and then
    /// dead-letter, leaving the run non-terminal the whole time. Signalled out
    /// of band for the same reason the spend breach is, and read by the handler
    /// the same way.
    permanent: Arc<std::sync::atomic::AtomicBool>,
    client: Arc<Client>,
    pool: PgPool,
    guard: SpendGuard,
    purpose: Purpose,
    model: String,
    agent_id: Option<Uuid>,
    run_id: Option<Uuid>,
}

impl Adapter {
    #[must_use]
    pub fn new(
        client: Arc<Client>,
        pool: PgPool,
        guard: SpendGuard,
        purpose: Purpose,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client,
            pool,
            guard,
            purpose,
            model: model.into(),
            agent_id: None,
            run_id: None,
            permanent: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn for_agent(mut self, agent_id: Uuid) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    #[must_use]
    pub fn for_run(mut self, run_id: Uuid) -> Self {
        self.run_id = Some(run_id);
        self
    }

    /// The guard this adapter trips on a spend breach.
    ///
    /// The handler must clone this **before** the adapter is moved into
    /// `Engine::new`, which takes its `Llm` by value.
    #[must_use]
    pub fn guard(&self) -> SpendGuard {
        self.guard.clone()
    }

    /// A flag the adapter sets when the provider refused permanently.
    ///
    /// Same lifetime rule as [`Self::guard`]: clone it before the adapter moves.
    #[must_use]
    pub fn permanent_failure_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.permanent.clone()
    }

    fn record_for(&self, tokens: Tokens, model: &str, error: Option<&str>) -> Record {
        let account = self.guard.account_id();
        let model = if model.is_empty() {
            self.model.as_str()
        } else {
            model
        };
        let mut record = match error {
            None => Record::ok(account, self.purpose, model, tokens),
            Some(message) => Record::failed(account, self.purpose, model, tokens, message),
        };
        record.agent_id = self.agent_id;
        record.run_id = self.run_id;
        record
    }

    async fn record(&self, record: &Record) {
        ledger::record_or_log(&self.pool, record).await;
    }
}

#[async_trait]
impl Llm for Adapter {
    async fn chat(&self, req: ChatRequest) -> SdkResult<ChatResponse> {
        // Before the call, never after: after is too late to prevent the spend
        // this check exists to prevent.
        match self.guard.check().await {
            Ok(Ok(())) => {}
            Ok(Err(breach)) => {
                // The flag is now set. The job handler reads it and cancels the
                // run rather than handing the job back to the queue, which
                // would retry and spend again.
                return Err(SdkError::llm(breach.to_string()));
            }
            Err(err) => {
                return Err(SdkError::llm(format!(
                    "could not read the spend ledger: {err}"
                )))
            }
        }

        let wire =
            to_wire(&req, &self.model, DEFAULT_MAX_OUTPUT_TOKENS).map_err(Error::into_sdk)?;
        let model = wire.model.clone();

        let sent = self.client.send(&wire).await;
        let response = match sent {
            Ok(response) => response,
            Err(err) => {
                // A call that failed is still a call. Recording it keeps a
                // burst of failures visible in the ledger instead of absent
                // from it - and a provider that billed a partial stream still
                // billed it.
                if matches!(err, Error::Permanent { .. } | Error::Translation(_)) {
                    // No retry will change the answer. The handler cancels the
                    // run rather than handing the job back to the queue.
                    self.permanent
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                let message = err.to_string();
                self.record(&self.record_for(Tokens::default(), &model, Some(&message)))
                    .await;
                return Err(err.into_sdk());
            }
        };

        // Read the counters *before* `from_wire` consumes the response. A body
        // that will not decode was still billed, and pricing that path at zero
        // - which is what `Tokens::default()` did here - under-counts the
        // ceiling by exactly the calls that went wrong. `WireResponse::usage`
        // exists for this, and the compiler was already using it.
        let billed = response.usage();
        match from_wire(response) {
            Ok((chat, tokens, reported_model)) => {
                // Prefer the model the provider says answered: an alias can
                // resolve to a dated snapshot, and the ledger should record
                // what actually ran.
                let effective = if reported_model.is_empty() {
                    model
                } else {
                    reported_model
                };
                self.record(&self.record_for(tokens, &effective, None))
                    .await;
                Ok(chat)
            }
            Err(err) => {
                let message = err.to_string();
                self.record(&self.record_for(billed, &model, Some(&message)))
                    .await;
                Err(err.into_sdk())
            }
        }
    }
}

#[cfg(test)]
mod tests;
