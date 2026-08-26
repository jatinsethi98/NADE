//! `write_note` — the first tool that writes anything.

use async_trait::async_trait;
use durable_agent::{Error as SdkError, Result as SdkResult, Tool, ToolCall};
use serde_json::{json, Value};

use super::{effect_identity, optional_str, required_str, ToolContext};
use crate::agents::fence;

/// `API.md` §0 caps a note body at 256 KB.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Titles are a list row, not a document.
const MAX_TITLE_BYTES: usize = 500;

#[derive(Debug)]
pub struct WriteNote {
    context: ToolContext,
}

impl WriteNote {
    #[must_use]
    pub const fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl Tool for WriteNote {
    fn name(&self) -> &str {
        "write_note"
    }

    fn description(&self) -> Option<&str> {
        // C1/C2: the copy must describe what actually happens. A note is saved
        // in NADE. Nothing is sent anywhere.
        Some(
            "Save a note in NADE for the user to read later. The note is stored in this app \
             only; it is not emailed or shared with anyone.",
        )
    }

    fn version(&self) -> Option<&str> {
        Some("1")
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "A short title for the note."},
                "body_md": {"type": "string", "description": "The note body, in Markdown."},
                "thread_id": {
                    "type": "string",
                    "description": "Optional: the email thread this note is about."
                }
            },
            "required": ["title", "body_md"],
            "additionalProperties": false
        })
    }

    /// Gated when the agent is. Pure in the call, as the SDK requires: the
    /// answer comes from the agent's setting baked in at construction, never
    /// from the arguments and never from a fresh read.
    fn requires_approval(&self, _call: &ToolCall) -> bool {
        self.context.approval_required
    }

    async fn execute(&self, call: &ToolCall) -> SdkResult<Value> {
        let (run_id, effect_id) = effect_identity(call)?;
        let title = required_str(&call.arguments, "title")?;
        let body = required_str(&call.arguments, "body_md")?;
        let thread_id = super::owned_thread_id(
            &self.context.state,
            self.context.account.id,
            optional_str(&call.arguments, "thread_id"),
        )
        .await;

        // Control characters are stripped before anything else: a NUL anywhere
        // in this row - or in the result that goes back into `run_journal` -
        // would be rejected by `jsonb` and strand the run mid-step, with the
        // note already written.
        let title = super::cap(&fence::strip_control_characters(title), MAX_TITLE_BYTES);
        let body = super::cap(&fence::strip_control_characters(body), MAX_BODY_BYTES);

        // `on conflict (id) do update`, never a plain insert and never a fresh
        // id. `API.md` §6.2: "Human approval is not an idempotency mechanism";
        // the same `effect_id` on every attempt is what makes a re-execution
        // after a crash collapse onto one row.
        sqlx::query(
            "insert into notes (id, account_id, run_id, thread_id, title, body_md, unread) \
             values ($1, $2, $3, $4, $5, $6, true) \
             on conflict (id) do update set \
               title = excluded.title, \
               body_md = excluded.body_md, \
               thread_id = excluded.thread_id, \
               updated_at = now()",
        )
        .bind(effect_id)
        .bind(self.context.account.id)
        .bind(run_id)
        .bind(thread_id.as_deref())
        .bind(&title)
        .bind(&body)
        .execute(&self.context.state.pool)
        .await
        .map_err(|err| SdkError::tool_source("could not save the note", err))?;

        // Deterministic: the same bytes on every attempt at this step.
        Ok(json!({"note_id": effect_id, "saved": true, "title": title}))
    }
}
