//! The feed producer: turning what a run did into the card that asks about it.
//!
//! # One author for `feed_items`
//!
//! Every card in the product is written here — [`raise_approval`] for a gated
//! step, [`raise_run_info`] for what a run produced, and [`raise_notice`] for
//! the two the system raises about itself (`run::raise_spend_ceiling_notice`,
//! `gmail::oauth`'s needs-reauth card). That is not tidiness: `data` is served
//! **verbatim** by `GET /feed`, and `FEED_DATA` in `docs/contract/validate.py`
//! is an *exact key set* — "a missing key and an extra key are both
//! violations". Both system writers had grown a fifth key, and nothing could
//! notice while `/feed` was unmounted. A single author is what makes a shape
//! change one edit.
//!
//! The exceptions, and they are deliberate: `api::feed::settle_card` and
//! `run::settle_cards_of` *resolve* cards rather than raising them, and each
//! belongs to the transaction that decides the card's fate.
//!
//! # The card is stamped by the engine's clock, not by ours
//!
//! `docs/contract/validate.py` ties three timestamps together: the card's
//! `created_at` **equals** the `approval_requested` journal entry's, and
//! `approval_expires_at` **equals** that plus seven days. So the producer reads
//! the gate entry back out of `run_journal` and binds its `created_at`
//! explicitly, rather than letting `feed_items.created_at`'s `default now()`
//! stamp settle-time. `ApprovalRequest::requested_at` is *not* the same value —
//! it is the step's `opened_at`, and `Entry::new` reads the clock a second time
//! when it builds the entry — so the entry is the only source that satisfies
//! the rule.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use nade_agent_sdk::ApprovalRequest;
use serde_json::{json, Value};
use sqlx::{PgExecutor, Postgres, Transaction};
use uuid::Uuid;

use super::{fence, tools, tools::draft_reply};

/// `API.md` §7: "Approvals expire **7 days** after creation."
pub const APPROVAL_TTL_DAYS: i64 = 7;

/// The card body is a sentence, not a transcript.
const MAX_BODY: usize = 400;

/// `API.md` §2's one-line summary under a mail row.
const MAX_AGENT_NOTE: usize = 120;

/// A note title on a card, which the design renders on one line.
const MAX_NOTE_TITLE: usize = 200;

/// Verbs that would make a card **lie about what v1 does**.
///
/// This screens the *model's* prose about somebody's mail, not copy we author,
/// so it is deliberately not `docs/contract/validate.py`'s `OUTBOUND_VERBS`.
/// The line between them is a tense, not a topic:
///
/// * **screened** — every form that claims NADE *did* something outbound, in
///   any inflection. "Sent your reply", "Emailed the invoice", "Replied on
///   your behalf", "Unsubscribing you from 40 lists". A card saying one of
///   these is a C1/C2 violation whoever caused it.
/// * **not screened** — the present-tense nouns and verbs that describe what
///   is *in* the message: "Draft a reply proposing Tuesday", "they want to
///   schedule an intro", "the invoice says to pay by Friday", "share the doc".
///   `OUTBOUND_VERBS` holds `schedule`, `book`, `pay`, `accept`, `decline`,
///   `share`, `post`, `rsvp`, `mail` and bare `reply`/`email` for copy we
///   write, where they would be promises; screening them here would push most
///   honest cards to the fallback and tell the user nothing.
///
/// The first cut of this list had seventeen words and no past tense for four
/// of the families it did cover, which is the same hole four iOS guards had
/// (they matched `send(s|ing)?` and not `sent`, so shipped copy reading "before
/// it could be sent" passed all of them).
const PROMISES_AN_OUTBOUND_ACTION: &[&str] = &[
    // Sending.
    "send",
    "sends",
    "sending",
    "sent",
    "resend",
    "resent",
    // Forwarding.
    "forward",
    "forwards",
    "forwarding",
    "forwarded",
    // Replying — the past and progressive only. "Draft a reply" is the
    // product; "replied" is a claim it already went out.
    "replied",
    "replying",
    "reply-all",
    "replied-all",
    // Mailing, as an act rather than as a noun.
    "emailed",
    "emailing",
    "mailed",
    "mailing",
    // Filing and destroying, neither of which v1 can do.
    "archive",
    "archives",
    "archiving",
    "archived",
    "delete",
    "deletes",
    "deleting",
    "deleted",
    "trash",
    "trashed",
    "trashing",
    // Acting on the sender's behalf.
    // Not bare `unsubscribe`/`unsubscribes`: "there is an unsubscribe link at
    // the bottom" is the single most ordinary honest thing a card can say
    // about marketing mail. Same tense rule as `reply` and `email`.
    "unsubscribing",
    "unsubscribed",
    "published",
    "publishing",
    "posted",
    "posting",
    "shared",
    "sharing",
];

/// Everything the product says about one gated tool, in one struct.
///
/// # Why this is a table and not five `match`es
///
/// A gate produces copy in five places: `data.action` and `data.action_label`
/// on the card, the `agent_note` under the mail row, the card's fallback
/// sentence, the italic line `POST /feed/{id}/approve` writes when it settles,
/// and whether §7's `actions` carries `edit`. Each of those was its own
/// `match request.tool { … _ => the note shape }`, in two modules — and the
/// `_` arm meant a tool nobody had taught them about did not fail, it
/// *silently* rendered as a note: "Saved to Notes." under a card that saved
/// something else. D78 is the record of a copy bug of exactly this kind
/// shipping, and the arms could not disagree only because they all collapsed
/// to the same default.
///
/// Adding a tool now touches one table, and [`tests::every_gated_tool_has_a_presentation`]
/// fails if it does not — which is the property five `_` arms cannot have.
pub struct GatePresentation {
    /// `API.md` §7.1's discriminator, written to `data.action` and read back by
    /// `FeedRow::into_wire`.
    pub action: &'static str,
    /// The verb on the card's primary button. Never "Send": the draft lives in
    /// NADE and never in Gmail (PLAN C1/C2).
    pub action_label: &'static str,
    /// The italic line under the card once it is approved (`API.md` §7).
    pub resolved_note: &'static str,
    /// `agent_note`'s second half — "{agent name} · {phrase}".
    pub note_phrase: &'static str,
    /// Whether §7's `actions` carries `edit`. Edit means "approve, then edit
    /// the draft it created" (deviation 54), so only a tool that *makes* a
    /// draft has one.
    pub offers_edit: bool,
    /// The card's sentence when the model's own is unusable and the call
    /// carries nothing quotable.
    pub fallback_body: &'static str,
}

const WRITE_NOTE: GatePresentation = GatePresentation {
    action: "write_note",
    action_label: "Save note",
    resolved_note: "Saved to Notes.",
    note_phrase: "a note to approve",
    offers_edit: false,
    fallback_body: "The agent has a note ready. Save it?",
};

const DRAFT_REPLY: GatePresentation = GatePresentation {
    action: "draft_reply",
    action_label: "Save draft",
    resolved_note: "Saved to Drafts.",
    note_phrase: "a draft reply to approve",
    offers_edit: true,
    fallback_body: "The agent has a draft reply ready. Save it?",
};

/// The presentation for a gated tool, or `None` for a name this build has none
/// for.
#[must_use]
pub fn presentation(tool: &str) -> Option<&'static GatePresentation> {
    match tool {
        "write_note" => Some(&WRITE_NOTE),
        "draft_reply" => Some(&DRAFT_REPLY),
        _ => None,
    }
}

/// [`presentation`], falling back to the shape that promises least.
///
/// An unknown tool reaching a card is a bug in whoever added it — the test
/// above is what catches it — but a run already parked on one must still
/// render, and `write_note` is the presentation that claims the least about
/// what approving it would do.
#[must_use]
pub fn presentation_or_default(tool: &str) -> &'static GatePresentation {
    presentation(tool).unwrap_or(&WRITE_NOTE)
}

/// The `data` of every `action: "none"` card — `API.md` §7.1's third shape.
///
/// `draft_id` is there because an agent with `approval_required = false` and
/// `draft_reply` in its tools is reachable: `Tool::requires_approval` answers
/// from the agent's setting, so such a run writes its draft with no card to
/// approve, and the `info` card that follows had no field to name it with.
#[must_use]
pub fn info_data(note_id: Option<Uuid>, draft_id: Option<Uuid>, thread_id: Option<&str>) -> Value {
    json!({
        "action": "none",
        "note_id": note_id,
        "draft_id": draft_id,
        "thread_id": thread_id,
    })
}

/// How often a system notice may be raised again while one is already live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OncePer {
    /// Never a second one until the first resolves. The needs-reauth card: it
    /// is not dismissible, so an unresolved one is on screen until re-consent.
    Ever,
    /// Once per UTC day, the same window the ledger counts in. The spend
    /// ceiling resets at midnight, so yesterday's card must not suppress
    /// today's.
    UtcDay,
}

/// Raise one of the cards the **system** raises about itself.
///
/// # Why the statement lives here
///
/// The module doc above claims one author for `feed_items`. It was half true:
/// `info_data` was shared and the statements were not, so
/// `run::raise_spend_ceiling_notice` and `gmail::oauth`'s needs-reauth card
/// each typed out their own `insert … select … where not exists (…)` — the same
/// once-per-reason guard, differing only in a `created_at` window and in
/// whether `dismissible` was stated or left to the column default. That split
/// is exactly how D72's contract breach came to live in one writer and not the
/// other, and a third system card (P6's push failure, P7's schedule error)
/// would have copied whichever it found first.
///
/// # Errors
/// Returns an error if the statement fails.
pub async fn raise_notice<'e, E>(
    executor: E,
    account_id: Uuid,
    reason: &str,
    title: &str,
    body: &str,
    dismissible: bool,
    once_per: OncePer,
) -> sqlx::Result<()>
where
    E: PgExecutor<'e>,
{
    // EDGE (duplicate delivery): the same failure repeating must not spam the
    // feed, so the row is written only when no live one has this reason.
    // `feed_items_reason_idx` is `(account_id, reason) where reason is not null
    // and status = 'new'` — `reason = $5` is strict, so the planner can use it.
    let window = match once_per {
        OncePer::Ever => String::new(),
        OncePer::UtcDay => {
            format!(
                " and created_at >= {}",
                crate::llm::ledger::SINCE_UTC_MIDNIGHT
            )
        }
    };
    sqlx::query(&format!(
        "insert into feed_items \
             (account_id, kind, title, body, data, status, reason, dismissible) \
         select $1, 'info', $2, $3, $4::jsonb, 'new', $5, $6 \
          where not exists ( \
              select 1 from feed_items \
               where account_id = $1 and kind = 'info' and status = 'new' \
                 and reason = $5{window})"
    ))
    .bind(account_id)
    .bind(title)
    .bind(body)
    .bind(info_data(None, None, None))
    .bind(reason)
    .bind(dismissible)
    .execute(executor)
    .await?;
    Ok(())
}

/// Resolve the live system notice for a reason, if there is one.
///
/// **No `resolved_note`.** `API.md` §7 is explicit that the italic line is set
/// for `resolved`, `skipped` and `expired` and is "null for `new` and for
/// `info`", and `docs/contract/validate.py` enforces it — so the 'Reconnected.'
/// the re-consent path used to write would have served an item in breach of its
/// own contract the day `/feed` was mounted. The card resolving *is* the
/// message.
///
/// # Errors
/// Returns an error if the statement fails.
pub async fn resolve_notice<'e, E>(executor: E, account_id: Uuid, reason: &str) -> sqlx::Result<()>
where
    E: PgExecutor<'e>,
{
    sqlx::query(
        "update feed_items set status = 'resolved' \
          where account_id = $1 and kind = 'info' and status = 'new' and reason = $2",
    )
    .bind(account_id)
    .bind(reason)
    .execute(executor)
    .await?;
    Ok(())
}

/// What the run's journal says about the pause the engine just reported.
#[derive(Debug, Default)]
pub struct GateContext {
    /// The `approval_requested` entry's own `created_at`. The card's identity
    /// in time.
    pub opened_at: Option<DateTime<Utc>>,
    /// The prose from the turn that made the gated call, if it said anything.
    pub prose: Option<String>,
}

/// Read the gate entry and the model's last words in one round trip.
///
/// `distinct on (kind)` over a run's own journal: two rows at most, and the
/// `(run_id, seq)` primary key is the index it walks. The newest
/// `model_response` **is** the gating turn — `approval_requested` is the last
/// entry in the journal when this is called, so nothing can have spoken since.
///
/// # Errors
/// Returns an error if the query fails.
pub async fn gate_context<'e, E>(executor: E, run_id: Uuid) -> Result<GateContext>
where
    E: PgExecutor<'e>,
{
    let rows: Vec<(String, Value, DateTime<Utc>)> = sqlx::query_as(
        "select distinct on (kind) kind, payload, created_at \
           from run_journal \
          where run_id = $1 and kind in ('approval_requested', 'model_response') \
          order by kind, seq desc",
    )
    .bind(run_id)
    .fetch_all(executor)
    .await?;

    let mut context = GateContext::default();
    for (kind, payload, created_at) in rows {
        match kind.as_str() {
            "approval_requested" => context.opened_at = Some(created_at),
            "model_response" => {
                context.prose = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    Ok(context)
}

/// The sentence a card shows, given what the model said and what it asked for.
///
/// Three ways a card gets its body, in order:
///
/// 1. the model's own prose, scrubbed and capped — what the design's cards are;
/// 2. the **fallback**, when the turn carried no text at all, which is the
///    ordinary case for a turn whose whole content is a tool call
///    (`API.md` §6.1: "`text` may be absent");
/// 3. the fallback again, when the prose claims an outbound action. That prose
///    came out of a model that just read somebody else's email, and a card
///    reading "Sent your reply" is a C1/C2 violation whoever caused it.
#[must_use]
pub fn card_body(prose: Option<&str>, request: &ApprovalRequest) -> String {
    screened(prose, || fallback_body(request))
}

/// The C1/C2 screen on model prose, and the one place it lives.
///
/// Both card producers ran this policy — cap, trim, reject empty, reject an
/// outbound claim, else fall back — and they ran it in different orders, one of
/// them without the trim. Two spellings of a safety screen is one spelling and
/// one near-miss. The fallback is a closure because on the happy path it is
/// never needed, and `fallback_body` allocates.
fn screened(prose: Option<&str>, fallback: impl FnOnce() -> String) -> String {
    prose
        .map(|prose| fence::stored(prose, MAX_BODY).trim().to_owned())
        .filter(|prose| !prose.is_empty() && !promises_an_outbound_action(prose))
        .unwrap_or_else(fallback)
}

/// The card's sentence when the model's own is unusable.
///
/// Pinned by a test rather than left to drift: it is contract-visible prose,
/// and D67 is the record of what happens to a rendered string nothing compares.
#[must_use]
pub fn fallback_body(request: &ApprovalRequest) -> String {
    let Some(presentation) = presentation(&request.tool) else {
        // A tool with no table entry. Naming it is more honest than the default
        // presentation's sentence, which would call it a note.
        return format!("The agent wants to run {}.", request.tool);
    };
    // `write_note` is the one gate that can quote what it is about.
    if request.tool == "write_note" {
        let title = note_title(request);
        if !title.is_empty() {
            return format!("The agent has a note ready: “{title}”. Save it?");
        }
    }
    presentation.fallback_body.to_owned()
}

/// Does this sentence claim NADE did something outbound?
///
/// Word-boundary matching over lowercased ASCII words, so "resent" and
/// "senders" do not trip a screen meant for "sent" and "send". Non-ASCII is
/// preserved by splitting on character class rather than by stripping.
#[must_use]
pub fn promises_an_outbound_action(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .any(|word| PROMISES_AN_OUTBOUND_ACTION.contains(&word))
}

/// `API.md` §2's `agent_note`: "exactly the string the mail row renders under
/// the snippet".
///
/// Rendered from the pending action, not from the model's prose. The design's
/// own fixture reads "Reply Drafter · a draft reply to approve", which is this
/// function; its other one ("… · two next steps to approve") described the
/// note's contents, which no server can reproduce from a tool call. The fixture
/// moved to what is derivable rather than this drifting from the fixture.
#[must_use]
pub fn agent_note(agent_name: &str, tool: &str) -> String {
    let phrase = presentation(tool).map_or("something to approve", |p| p.note_phrase);
    tools::cap(&format!("{agent_name} · {phrase}"), MAX_AGENT_NOTE)
}

/// The note title a `write_note` gate carries, capped for display.
fn note_title(request: &ApprovalRequest) -> String {
    request
        .call
        .arguments
        .get("title")
        .and_then(Value::as_str)
        .map(|title| fence::stored(title, MAX_NOTE_TITLE))
        .unwrap_or_default()
}

/// The thread a gated call names, if it names one.
#[must_use]
pub fn gate_thread_id(request: &ApprovalRequest) -> Option<String> {
    request
        .call
        .arguments
        .get("thread_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// `API.md` §7.1's `data`, typed by the tool the gate holds.
///
/// `never_messaged` is passed in rather than queried here: it reads a table mail
/// sync is concurrently writing, and the caller owns the transaction.
///
/// Sender-controlled text — the subject, the recipients, a note title the model
/// chose after reading somebody's mail — goes through [`fence::stored`], not
/// [`fence::field`]. The difference is deliberate and is the whole reason
/// `stored` exists: `field` also **neutralises** marker-shaped text, which is
/// right for a string about to enter a prompt and wrong for one about to be
/// shown to a person. This `data` is rendered by the app as plain text, so
/// mangling `<<<NADE-UNTRUSTED-DATA` in a subject line would corrupt what the
/// user reads for no gain. What both share is the part that is not optional:
/// control characters never reach `jsonb`, which rejects a NUL outright (D29).
#[must_use]
pub fn approval_data(request: &ApprovalRequest, never_messaged: bool) -> Value {
    let thread_id = gate_thread_id(request);
    let presentation = presentation_or_default(&request.tool);
    match presentation.action {
        "draft_reply" => {
            let to: Vec<String> = request
                .call
                .arguments
                .get("to")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        // **Capped here, not only at execution.**
                        // `draft_reply` enforces `MAX_RECIPIENTS` when it runs,
                        // which is *after* the human decides. A coerced model
                        // could put two hundred addresses on the card, and the
                        // card renders them on two truncated lines — so the
                        // control `backend/testdata/injection` finding 10
                        // depends on ("the card shows the actual recipient
                        // list") would hide the middle of it. It also stops the
                        // card asking for a call the tool would refuse.
                        .take(draft_reply::MAX_RECIPIENTS)
                        .map(|address| fence::stored(address, tools::MAX_ADDRESS_BYTES))
                        .collect()
                })
                .unwrap_or_default();
            let subject = request
                .call
                .arguments
                .get("subject")
                .and_then(Value::as_str)
                .map(|subject| fence::stored(subject, tools::MAX_SUBJECT_BYTES))
                .unwrap_or_default();
            json!({
                "action": presentation.action,
                "action_label": presentation.action_label,
                "draft_id": request.effect_id,
                "thread_id": thread_id,
                "to": to,
                "subject": subject,
                "never_messaged": never_messaged,
            })
        }
        // `write_note`, and anything `presentation_or_default` fell back to.
        _ => json!({
            "action": presentation.action,
            "action_label": presentation.action_label,
            "note_title": note_title(request),
            "note_id": request.effect_id,
            "thread_id": thread_id,
        }),
    }
}

/// Raise (or re-raise) the approval card for a gated step.
///
/// Idempotent on `(run_id, step_seq)`: `Engine::run` is safe to call repeatedly
/// and answers a parked run by replay, so this is reached again on every retry
/// of the run job. The unique index is what makes the second call a no-op
/// rather than a second card with a second token — and the token is a
/// capability, so minting a fresh one per retry would be a live one per retry.
///
/// # Errors
/// Returns an error if any statement fails.
#[allow(clippy::too_many_arguments)]
pub async fn raise_approval(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    run_id: Uuid,
    agent_name: &str,
    request: &ApprovalRequest,
    context: &GateContext,
    never_messaged: bool,
) -> Result<()> {
    // No gate entry means the journal does not have the approval this outcome
    // claims. Raising a card stamped `now()` would violate the contract rule
    // that ties the two together, so this refuses instead — the run stays
    // parked and the job's next attempt reads a committed journal.
    let Some(created_at) = context.opened_at else {
        anyhow::bail!("run {run_id} reported an approval with no `approval_requested` entry");
    };
    let expires_at = created_at + Duration::days(APPROVAL_TTL_DAYS);
    let data = approval_data(request, never_messaged);
    let thread_id = gate_thread_id(request);
    let step_seq = i32::try_from(request.step_seq).unwrap_or(i32::MAX);

    sqlx::query(
        "insert into feed_items \
             (account_id, run_id, kind, title, body, data, status, \
              approval_token, approval_expires_at, step_seq, thread_id, agent_note, created_at) \
         values ($1, $2, 'approval', $3, $4, $5::jsonb, 'new', \
                 gen_random_uuid(), $6, $7, $8, $9, $10) \
         on conflict (run_id, step_seq) where run_id is not null and step_seq is not null \
         do nothing",
    )
    .bind(account_id)
    .bind(run_id)
    .bind(tools::cap(agent_name, MAX_NOTE_TITLE))
    .bind(card_body(context.prose.as_deref(), request))
    .bind(&data)
    .bind(expires_at)
    .bind(step_seq)
    .bind(thread_id.as_deref())
    // `agent_note` is what the *mail list* renders, so a card with no thread
    // has nothing to hang one on.
    .bind(
        thread_id
            .as_ref()
            .map(|_| agent_note(agent_name, &request.tool)),
    )
    .bind(created_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Raise the `info` card for a run that finished having written something
/// nobody had to approve.
///
/// This is the `approval_required = false` path, and the fixture world's
/// `feed_item_info.json` is exactly it. A run that wrote nothing gets no card:
/// there is nothing to tell anyone. Neither does a **failed** run — no fixture
/// describes such a card, and the Run log (P7) is where a failure belongs.
///
/// # And neither does a run that already asked
///
/// The first cut guarded on nothing but `Done`, which made this fire on the
/// **headline P5 flow**: card raised → approved → the resume job executes the
/// note → `Done` → a *second* card, `kind: "info"`, `status: "new"`. The feed
/// then said "Saved to Notes." and "The agent saved a note." about one note,
/// and `new_count` went back up the moment the user cleared it.
/// `docs/contract/feed.json` has exactly one card per run, so that was a shape
/// the design authority does not describe.
///
/// The guard is "has this run ever raised an approval", not
/// `agents.approval_required`: the setting can be turned off between the gate
/// and the resume, and what matters is whether the user was already told.
///
/// # Errors
/// Returns an error if any statement fails.
pub async fn raise_run_info(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    run_id: Uuid,
    agent_name: &str,
    summary: Option<&str>,
) -> Result<bool> {
    let already_asked: bool = sqlx::query_scalar(
        "select exists (select 1 from feed_items where run_id = $1 and kind = 'approval')",
    )
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?;
    if already_asked {
        return Ok(false);
    }

    // What the run actually produced, by the deterministic ids its steps wrote
    // under. Both are `on delete set null` from `agent_runs`, so a cancelled
    // agent's rows do not resurrect here.
    let note: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "select id, thread_id from notes where run_id = $1 order by created_at desc limit 1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;
    let draft: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "select id, thread_id from drafts where run_id = $1 order by created_at desc limit 1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;

    if note.is_none() && draft.is_none() {
        return Ok(false);
    }

    let thread_id = note
        .as_ref()
        .and_then(|(_, thread)| thread.clone())
        .or_else(|| draft.as_ref().and_then(|(_, thread)| thread.clone()));
    let data = info_data(
        note.as_ref().map(|(id, _)| *id),
        draft.as_ref().map(|(id, _)| *id),
        thread_id.as_deref(),
    );

    // The same screen the approval card's body gets, and deliberately the same
    // function: this chain used to run the three checks in its own order and
    // store the result untrimmed.
    let body = screened(summary, || default_info_body(note.is_some()));

    // An `info` card carries no `agent_note`: `API.md` §2 says the mail row's
    // note belongs to a run that "still wants something", and this one is done.
    sqlx::query(
        "insert into feed_items \
             (account_id, run_id, kind, title, body, data, status, thread_id) \
         values ($1, $2, 'info', $3, $4, $5::jsonb, 'new', $6)",
    )
    .bind(account_id)
    .bind(run_id)
    .bind(tools::cap(agent_name, MAX_NOTE_TITLE))
    .bind(body)
    .bind(&data)
    .bind(thread_id.as_deref())
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

fn default_info_body(wrote_note: bool) -> String {
    if wrote_note {
        "The agent saved a note.".to_owned()
    } else {
        "The agent saved a draft.".to_owned()
    }
}

#[cfg(test)]
mod tests;
