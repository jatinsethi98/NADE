//! `read_thread` — one conversation, fenced.

use async_trait::async_trait;
use durable_agent::{Error as SdkError, Result as SdkResult, Tool, ToolCall};
use serde_json::{json, Value};

use super::{
    required_str, ToolContext, MAX_ADDRESS_BYTES, MAX_RECIPIENTS_SHOWN, MAX_SUBJECT_BYTES,
};
use crate::agents::fence;

/// The most messages shown, before the size budget gets a say.
const MAX_MESSAGES: usize = 8;

/// Per-message body budget, applied before fencing.
const MAX_BODY_BYTES: usize = 1_500;

/// The serialised result must fit under `EngineConfig::max_tool_result_bytes`
/// (16 KiB), or the SDK replaces the **whole** result with a truncation
/// envelope and the model gets a fragment of JSON instead of the thread.
///
/// A per-message byte budget is not enough to guarantee that, which is the
/// mistake this constant exists to correct. Every `\n` and `"` in a body costs
/// **two** bytes once serialised, and `strip_control_characters` deliberately
/// keeps newlines - so eight hard-wrapped messages with quoted replies measured
/// well over the cap while eight blocks of unbroken text measured under it. The
/// original guarding test used unbroken text and therefore passed on the one
/// input shape real mail never has.
///
/// So the projection is measured, not estimated: messages are dropped from the
/// oldest end until the encoded result fits.
const MAX_RESULT_BYTES: usize = 15_000;

#[derive(Debug)]
pub struct ReadThread {
    context: ToolContext,
}

impl ReadThread {
    #[must_use]
    pub const fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl Tool for ReadThread {
    fn name(&self) -> &str {
        "read_thread"
    }

    fn description(&self) -> Option<&str> {
        Some(
            "Read one email thread: its subject and the text of its messages. Thread ids come \
             from search_mail or from the message that triggered this run.",
        )
    }

    fn version(&self) -> Option<&str> {
        Some("1")
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": {"type": "string", "description": "The thread's id."}
            },
            "required": ["thread_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, call: &ToolCall) -> SdkResult<Value> {
        let thread_id = required_str(&call.arguments, "thread_id")?;

        // The same function `GET /threads/{id}` serves, so the cache
        // completion, the ordering and the attachment join cannot diverge
        // between the screen and the agent.
        let detail =
            crate::api::mail::thread_detail(&self.context.state, &self.context.account, thread_id)
                .await
                .map_err(|err| SdkError::tool(err.message.clone()))?;

        let total = detail.messages.len();
        let nonce = fence::nonce_for(call);

        // Newest first while trimming, then flipped back to oldest-first for
        // the model - `thread_detail` orders oldest-first, so the newest
        // messages are the tail and they are the ones worth keeping.
        let mut shown: Vec<Value> = detail
            .messages
            .iter()
            .rev()
            .take(MAX_MESSAGES)
            .map(|message| {
                json!({
                    "gmail_id": message.gmail_id,
                    // An address and a display name are as sender-controlled
                    // as the body is, and they land outside the fence - so they
                    // go through the same one-line policy the subject does.
                    "from": fence::field(&nonce, &message.from_email, MAX_ADDRESS_BYTES),
                    "from_name": fence::field(&nonce, &message.from_name, MAX_ADDRESS_BYTES),
                    "to": message
                        .to
                        .iter()
                        .take(MAX_RECIPIENTS_SHOWN)
                        .map(|address| fence::field(&nonce, address, MAX_ADDRESS_BYTES))
                        .collect::<Vec<_>>(),
                    "date": message.ts,
                    // The body is the hostile surface. Everything an attacker
                    // controls goes inside a labelled block that it cannot
                    // close.
                    "body": fence::fence(
                        &nonce,
                        "email body",
                        &super::cap(&message.body_text, MAX_BODY_BYTES),
                    ),
                })
            })
            .collect();
        shown.reverse();

        // Subject is sender-controlled and, unlike snippet, has no cap of its
        // own upstream: `mail::parse::MAX_SUBJECT` is 4 000 **characters**,
        // which is ~16 KB of astral codepoints - enough to blow the result cap
        // single-handed and destroy every message in it.
        let subject = fence::field(&nonce, &detail.subject, MAX_SUBJECT_BYTES);

        let build = |shown: &[Value]| {
            json!({
                "thread_id": detail.id,
                "subject": subject,
                "message_count": total,
                "returned": shown.len(),
                "truncated": total > shown.len(),
                // `API.md` §2: `partial` means "what you are reading has gaps".
                // The screen has to surface it and so does the model - an agent
                // that concludes "nobody replied" from a truncated thread is
                // worse than one that says it could not tell.
                "partial": detail.partial,
                "messages": shown,
            })
        };

        // Measured, not estimated. Drop the oldest until the encoded result
        // fits; the newest message is the one the agent is usually answering.
        while shown.len() > 1
            && serde_json::to_vec(&build(&shown)).map_or(0, |bytes| bytes.len()) > MAX_RESULT_BYTES
        {
            shown.remove(0);
        }

        Ok(build(&shown))
    }
}
