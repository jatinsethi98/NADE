//! `search_mail` — the whole mailbox, through Gmail.

use async_trait::async_trait;
use nade_agent_sdk::{Error as SdkError, Result as SdkResult, Tool, ToolCall};
use serde_json::{json, Value};

use super::{required_str, ToolContext, MAX_ADDRESS_BYTES, MAX_SUBJECT_BYTES};
use crate::agents::fence;

/// How many hits the model is shown.
///
/// `search::PAGE_SIZE` is 50, which is right for a screen and wrong for a
/// prompt: fifty rows of subject and snippet exceed the engine's 16 KiB tool
/// result cap on ordinary mail, so the model would receive a truncation
/// envelope instead of results. Ten is what fits and what an agent can reason
/// about; it can search again with a narrower query.
const MAX_HITS: usize = 10;

/// Snippets are already whitespace-collapsed and 200 characters (`API.md` §2).
/// Capped again here so a change there cannot silently blow the prompt budget.
const MAX_SNIPPET: usize = 200;

#[derive(Debug)]
pub struct SearchMail {
    context: ToolContext,
}

impl SearchMail {
    #[must_use]
    pub const fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl Tool for SearchMail {
    fn name(&self) -> &str {
        "search_mail"
    }

    fn description(&self) -> Option<&str> {
        Some(
            "Search the user's whole mailbox with a Gmail search query, for example \
             `from:acme.com newer_than:7d` or `\"invoice\" has:attachment`. Returns matching \
             threads, newest first. Use read_thread to read one of them.",
        )
    }

    fn version(&self) -> Option<&str> {
        // Bumped when the projection or the caps change, so a step opened under
        // one shape never executes under another.
        Some("1")
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A Gmail search query. Operators like from:, to:, subject:, \
                                    has:attachment, newer_than:7d and label:NAME are supported."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, call: &ToolCall) -> SdkResult<Value> {
        let query = required_str(&call.arguments, "query")?;

        // `search::search` and not a second implementation: `search/mod.rs`
        // says so outright, and the traps it already handles are the ones a
        // model walks straight into - Gmail answers a malformed query with an
        // empty 200, and `label:` takes a name where a program has an id.
        let page = crate::search::search(
            &self.context.state,
            Some(self.context.account.id),
            query,
            None,
        )
        .await;

        let page = match page {
            Ok(page) => page,
            Err(err) => {
                // The rejection text is the useful part. An agent told
                // "unknown operator `label:Label_25`; use the label name" can
                // correct itself; one handed an empty list cannot tell a bad
                // query from a mailbox with no matches.
                //
                // A dead credential is different: no retry and no rephrasing
                // fixes it, so the model is told to stop rather than to try
                // again, and the run ends without burning its remaining steps.
                let message = if err.code == crate::error::ErrorCode::NeedsReauth {
                    format!(
                        "{} Mail search is unavailable until then; do not retry this tool.",
                        err.message
                    )
                } else {
                    err.message.clone()
                };
                return Err(SdkError::tool(message));
            }
        };

        let nonce = fence::nonce_for(call);
        let total = page.threads.len();
        let hits: Vec<Value> = page
            .threads
            .iter()
            .take(MAX_HITS)
            .map(|thread| {
                json!({
                    "thread_id": thread.id,
                    // Subject, sender and snippet are all sender-controlled.
                    // They are short and structural here rather than fenced as
                    // a block - the fence belongs around message bodies in
                    // `read_thread` - but every one of them goes through
                    // `fence::field`, which scrubs, de-fangs and caps in the
                    // one order that is safe. `snippet` is 200 chars upstream;
                    // `subject` is 4 000 **characters**, ~16 KB of astral
                    // codepoints, on its own enough to blow the engine's result
                    // cap and destroy every hit in the same response.
                    "subject": fence::field(&nonce, &thread.subject, MAX_SUBJECT_BYTES),
                    "from": fence::field(&nonce, &thread.from_email, MAX_ADDRESS_BYTES),
                    "date": thread.ts,
                    "snippet": fence::field(&nonce, &thread.snippet, MAX_SNIPPET),
                })
            })
            .collect();

        Ok(json!({
            "query": query,
            "returned": hits.len(),
            // Stated, never silent: PLAN.md's "no silent caps" applies to what
            // the model is shown as much as to what a human is.
            "truncated": total > hits.len(),
            "threads": hits,
        }))
    }
}
