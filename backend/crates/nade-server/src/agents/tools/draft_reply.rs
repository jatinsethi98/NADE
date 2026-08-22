//! `draft_reply` — prepares a reply. Never sends one.
//!
//! v1 takes **no outbound actions** (PLAN.md C1/C2). A draft is a NADE draft:
//! it lives in this database, it never reaches Gmail, and no copy in this file
//! may suggest otherwise.

use async_trait::async_trait;
use nade_agent_sdk::{Error as SdkError, Result as SdkResult, Tool, ToolCall};
use serde_json::{json, Value};

use super::{effect_identity, optional_str, required_str, ToolContext};
use crate::agents::fence;

/// `pub(crate)` for the same reason [`valid_address`] is: `PATCH /drafts/{id}`
/// enforces the identical caps, so that an edit cannot produce a draft an agent
/// could not have written. Two copies of the numbers would drift the first time
/// one of them moved.
pub(crate) const MAX_BODY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SUBJECT_BYTES: usize = 500;

/// More than this and it is not a reply.
pub(crate) const MAX_RECIPIENTS: usize = 20;

#[derive(Debug)]
pub struct DraftReply {
    context: ToolContext,
}

impl DraftReply {
    #[must_use]
    pub const fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

/// Addresses this mailbox has never corresponded with.
///
/// PLAN.md's injection defences list "never-messaged recipients flagged red".
/// Exfiltration-by-draft is a standing attack in the corpus - the agent is
/// talked into addressing a reply to the attacker - and the signal a human
/// needs on the approval card is exactly this: *you have never written to this
/// person before*.
///
/// # Why this is not part of the tool's result
///
/// Two reasons, and both matter.
///
/// It would be **too late**. The approval card is raised from
/// `approval_requested`, which the engine appends *before* the tool executes
/// and *before* the human decides. A flag computed inside `execute` is only
/// available after the human has already approved, which is the one moment it
/// is worth nothing.
///
/// It would also be **non-deterministic**. The SDK requires a tool result to be
/// byte-identical on every attempt at a step - "a tool whose output depends on
/// the attempt number, on the wall clock, or on anything else outside the call
/// has broken the idempotency contract". This reads a table that mail sync is
/// concurrently writing, so two attempts at one step could legitimately
/// disagree.
///
/// So it lives here as a plain query, and P5 calls it when it raises the card,
/// with the `to` list out of `agent_runs.pending_action`.
///
/// A flag, never a block: the tool does not get to decide, the human does.
///
/// # Errors
/// Returns an error if the query fails. A caller that cannot reach the database
/// should show the warning rather than hide it - failing closed on a *warning*
/// means the quiet path is the unsafe one.
pub async fn never_messaged(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
    addresses: &[String],
) -> sqlx::Result<Vec<String>> {
    // EDGE (empty input): no addresses, no question to ask the database.
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    // One round trip for the whole list, not one per address. `unnest` turns
    // the bound array into rows and the correlated `exists` answers all twenty
    // in a single scan - the house idiom (`mail::cache`, `search`, `api::mail`
    // all bind arrays this way) rather than a loop that re-scans `messages`
    // once per recipient.
    //
    // Case-insensitive on **both** sides. `jsonb_exists` is an exact byte
    // match, so a reply to `Priya@Kettle.com` addressed as `priya@kettle.com`
    // was flagged "never messaged" - a warning the human learns to ignore is a
    // warning that no longer works.
    let seen: Vec<String> = sqlx::query_scalar(
        "select w.addr from unnest($2::text[]) as w(addr) \
          where exists ( \
            select 1 from messages m \
             where m.account_id = $1 \
               and (lower(m.from_email) = lower(w.addr) \
                    or exists (select 1 from jsonb_array_elements_text(m.to_json) a \
                                where lower(a) = lower(w.addr))))",
    )
    .bind(account_id)
    .bind(addresses)
    .fetch_all(pool)
    .await?;

    // Order follows the caller's list, not the database's: the card reads back
    // in the order the human sees the recipients.
    Ok(addresses
        .iter()
        .filter(|address| !seen.contains(address))
        .cloned()
        .collect())
}

/// `API.md` §3: "Each address in `to` must contain `@`".
///
/// `pub(crate)` because `PATCH /drafts/{id}` applies the same rule: an edit
/// must not be able to produce a draft an agent could not have written, and
/// two spellings of "is this an address" would drift the moment one moved.
///
/// Deliberately not a full RFC 5322 validator. The check exists so a model
/// cannot invent `to: ["everyone"]`, and a stricter one would reject addresses
/// that Gmail itself accepts.
pub(crate) fn valid_address(address: &str) -> bool {
    let address = address.trim();
    match address.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.contains('@')
                && !address.contains(' ')
        }
        None => false,
    }
}

#[async_trait]
impl Tool for DraftReply {
    fn name(&self) -> &str {
        "draft_reply"
    }

    fn description(&self) -> Option<&str> {
        // C1/C2 again, and this one matters most: nothing here may imply the
        // draft is sent. `docs/contract/validate.py` scans user-facing copy for
        // outbound verbs for the same reason.
        Some(
            "Prepare a draft reply and save it in NADE for the user to review. The draft is \
             stored in this app only: it is not sent, not delivered, and not added to Gmail.",
        )
    }

    fn version(&self) -> Option<&str> {
        Some("1")
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Email addresses the draft is addressed to."
                },
                "subject": {"type": "string", "description": "The draft's subject line."},
                "body_text": {"type": "string", "description": "The draft's plain-text body."},
                "thread_id": {
                    "type": "string",
                    "description": "Optional: the thread this draft replies to."
                }
            },
            "required": ["to", "subject", "body_text"],
            "additionalProperties": false
        })
    }

    fn requires_approval(&self, _call: &ToolCall) -> bool {
        self.context.approval_required
    }

    async fn execute(&self, call: &ToolCall) -> SdkResult<Value> {
        let (run_id, effect_id) = effect_identity(call)?;

        let recipients = call
            .arguments
            .get("to")
            .and_then(Value::as_array)
            .ok_or_else(|| SdkError::tool("`to` is required and must be an array of addresses"))?;
        if recipients.is_empty() {
            // EDGE: empty input. A draft addressed to nobody is not a draft.
            return Err(SdkError::tool("`to` must contain at least one address"));
        }
        if recipients.len() > MAX_RECIPIENTS {
            return Err(SdkError::tool(format!(
                "`to` has {} addresses; at most {MAX_RECIPIENTS} are allowed",
                recipients.len()
            )));
        }

        let mut addresses: Vec<String> = Vec::with_capacity(recipients.len());
        for entry in recipients {
            let address = entry
                .as_str()
                .map(str::trim)
                .ok_or_else(|| SdkError::tool("every entry in `to` must be a string"))?;
            let address = fence::strip_control_characters(address);
            if !valid_address(&address) {
                return Err(SdkError::tool(format!(
                    "{address:?} is not an email address; every entry in `to` must contain `@`"
                )));
            }
            addresses.push(address);
        }

        let subject = super::cap(
            &fence::strip_control_characters(required_str(&call.arguments, "subject")?),
            MAX_SUBJECT_BYTES,
        );
        let body = super::cap(
            &fence::strip_control_characters(required_str(&call.arguments, "body_text")?),
            MAX_BODY_BYTES,
        );
        let thread_id = super::owned_thread_id(
            &self.context.state,
            self.context.account.id,
            optional_str(&call.arguments, "thread_id"),
        )
        .await;

        sqlx::query(
            "insert into drafts (id, account_id, run_id, thread_id, to_json, subject, body_text) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             on conflict (id) do update set \
               to_json = excluded.to_json, \
               subject = excluded.subject, \
               body_text = excluded.body_text, \
               thread_id = excluded.thread_id, \
               updated_at = now()",
        )
        .bind(effect_id)
        .bind(self.context.account.id)
        .bind(run_id)
        .bind(thread_id.as_deref())
        .bind(Value::from(addresses.clone()))
        .bind(&subject)
        .bind(&body)
        .execute(&self.context.state.pool)
        .await
        .map_err(|err| SdkError::tool_source("could not save the draft", err))?;

        // Deterministic: the same bytes on every attempt at this step. The
        // never-messaged warning is deliberately NOT here - see
        // `never_messaged`, which P5 calls when it raises the card, at the only
        // moment the warning is worth anything.
        Ok(json!({
            "draft_id": effect_id,
            "saved": true,
            "to": addresses,
            "subject": subject,
        }))
    }
}

#[cfg(test)]
mod address_tests {
    use super::valid_address;

    #[test]
    fn an_address_needs_a_local_part_and_a_domain() {
        assert!(valid_address("priya@kettle.com"));
        assert!(valid_address("a+b@sub.example.co.uk"));
        assert!(!valid_address("everyone"));
        assert!(!valid_address("@kettle.com"));
        assert!(!valid_address("priya@"));
        assert!(!valid_address(""));
        assert!(!valid_address("a@b@c"));
        assert!(!valid_address("two words@x.com"));
    }
}
