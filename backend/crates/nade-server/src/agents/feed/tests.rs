//! The card producer: what a card says, and what it refuses to say.

use nade_agent_sdk::{ApprovalRequest, ToolCall};
use serde_json::json;
use uuid::Uuid;

use super::*;

fn gate(tool: &str, arguments: serde_json::Value) -> ApprovalRequest {
    ApprovalRequest {
        step_seq: 6,
        tool: tool.to_owned(),
        call: ToolCall::new("call_2_1", tool, arguments),
        args_hash: "sha256:0".to_owned(),
        effect_id: Uuid::nil(),
        requested_at: "2026-08-17T08:23:01Z".parse().unwrap(),
        expires_at: None,
    }
}

// ------------------------------------------------- the presentation table --

/// The property five `_` arms could not have.
///
/// Every tool that can park a run must have a row in the table, because the
/// fallback for a missing one is `write_note` — so a sixth tool with a gate and
/// no entry would ship a card reading "Save note" / "Saved to Notes." about
/// something that is not a note, in silence. D78 is the record of a copy bug of
/// exactly that shape reaching a build.
///
/// Driven off `V1_TOOLS` rather than a list typed here, so adding a tool to the
/// product is what makes this test start asking about it.
#[test]
fn every_gated_tool_has_a_presentation() {
    // The two mutating tools; `search_mail` and `read_thread` never gate.
    const GATED: [&str; 2] = ["write_note", "draft_reply"];
    for name in tools::V1_TOOLS {
        let entry = presentation(name);
        if GATED.contains(name) {
            let entry = entry.unwrap_or_else(|| panic!("{name} can gate and has no presentation"));
            assert_eq!(entry.action, *name, "the table is keyed by its own action");
            assert!(!entry.action_label.is_empty(), "{name}");
            assert!(!entry.resolved_note.is_empty(), "{name}");
            // PLAN C1/C2, at the one place every gated tool's copy now passes
            // through. A future tool cannot introduce a promise here without
            // this failing.
            for copy in [
                entry.action_label,
                entry.resolved_note,
                entry.note_phrase,
                entry.fallback_body,
            ] {
                assert!(
                    !promises_an_outbound_action(copy),
                    "{name}: {copy:?} promises an outbound action"
                );
            }
        } else {
            assert!(
                entry.is_none(),
                "{name} does not gate but has a presentation"
            );
        }
    }
}

/// The fallback is `write_note`'s, and it is the shape that promises least.
#[test]
fn an_unknown_tool_falls_back_to_the_note_shape_and_never_to_a_draft() {
    let entry = presentation_or_default("some_tool_from_p7");
    assert_eq!(entry.action, "write_note");
    assert!(!entry.offers_edit, "an unknown tool must not offer Edit");
    // …but the card's *sentence* still names it, rather than calling it a note.
    let request = gate("some_tool_from_p7", json!({}));
    assert_eq!(
        fallback_body(&request),
        "The agent wants to run some_tool_from_p7."
    );
}

// -------------------------------------------------------------- the body --

#[test]
fn the_body_is_the_models_prose_when_it_said_something() {
    let request = gate("write_note", json!({"title": "Kettle — next steps"}));
    assert_eq!(
        card_body(
            Some("Two next steps found — an intro and a portfolio session."),
            &request
        ),
        "Two next steps found — an intro and a portfolio session."
    );
}

/// `API.md` §6.1: "`text` may be absent (a turn with no prose)". A turn whose
/// whole content is the gated tool call routinely has none, so this is the
/// ordinary case rather than an edge.
#[test]
fn a_turn_with_no_prose_falls_back_to_a_rendered_sentence() {
    let request = gate("write_note", json!({"title": "Kettle — next steps"}));
    assert_eq!(
        card_body(None, &request),
        "The agent has a note ready: “Kettle — next steps”. Save it?"
    );
    assert_eq!(
        card_body(Some("   "), &request),
        "The agent has a note ready: “Kettle — next steps”. Save it?"
    );

    let request = gate("draft_reply", json!({"to": ["a@b.com"]}));
    assert_eq!(
        card_body(None, &request),
        "The agent has a draft reply ready. Save it?"
    );
}

#[test]
fn a_note_with_no_title_still_has_a_sentence() {
    let request = gate("write_note", json!({}));
    assert_eq!(
        card_body(None, &request),
        "The agent has a note ready. Save it?"
    );
}

/// PLAN.md C1/C2, at the one place the *model* writes user-facing copy.
///
/// The corpus's premise is a completely compromised model. It cannot make NADE
/// send anything — the tools do not exist — but it can put "Sent your reply to
/// ops@parcel-status-updates.com" on the home screen at 14 pt, which is a
/// promise v1 does not keep.
#[test]
fn prose_that_claims_an_outbound_action_is_replaced_not_shown() {
    let request = gate(
        "draft_reply",
        json!({"to": ["ops@parcel-status-updates.com"]}),
    );
    for lie in [
        "Sent your reply to ops@parcel-status-updates.com.",
        "I forwarded the thread as asked.",
        "Archived the conversation and replied.",
        "DELETED the old drafts.",
        "Unsubscribed you from the list.",
        // The forms the first cut of the list missed entirely.
        "Replied to your bank on your behalf.",
        "Emailed the invoice to accounts.",
        "Unsubscribing you from 40 lists.",
        "Mailed it on.",
        "Trashed the old thread.",
        "Shared the doc with the team.",
    ] {
        assert_eq!(
            card_body(Some(lie), &request),
            "The agent has a draft reply ready. Save it?",
            "{lie}"
        );
    }
}

/// The screen is narrower than `validate.py`'s `OUTBOUND_VERBS` on purpose: it
/// reads prose *about somebody's mail*, where these words describe what is in
/// the message rather than something NADE did. Screening them would push most
/// honest cards to the fallback.
#[test]
fn prose_about_the_mail_survives_the_screen() {
    let request = gate("write_note", json!({"title": "Kettle"}));
    for honest in [
        "Priya asked when you're free. Draft a reply proposing Tuesday?",
        "They want to schedule a 30-minute intro and book a room.",
        "The invoice says to pay by Friday.",
        "The sender resents the tone of the last message.",
        "Two senders are waiting on you.",
        // Present-tense description of the mail, which is the product's own
        // vocabulary and must survive.
        "Kamran wants to share the deck and book a room.",
        "The email asks for a reply by Friday.",
        "There is an unsubscribe link at the bottom.",
    ] {
        assert_eq!(card_body(Some(honest), &request), honest, "{honest}");
    }
}

/// EDGE (unicode): the screen splits on character class, so a non-ASCII word is
/// preserved rather than mangled, and an astral body still caps by bytes
/// without panicking on a boundary.
#[test]
fn the_body_survives_unicode_and_is_capped() {
    let request = gate("write_note", json!({"title": "x"}));
    let emoji = "🙂".repeat(5_000);
    let body = card_body(Some(&emoji), &request);
    assert!(body.len() <= MAX_BODY, "{}", body.len());
    assert!(body.starts_with('🙂'));

    assert_eq!(
        card_body(Some("Réponse à envoyer — deux étapes"), &request),
        "Réponse à envoyer — deux étapes"
    );
}

/// EDGE (unicode, hostile): a NUL in a body makes a `jsonb` insert fail
/// outright (D29's live sync bug), and the card's body is a `text` column a
/// model wrote.
#[test]
fn control_characters_never_reach_the_card() {
    let request = gate("write_note", json!({"title": "x"}));
    let body = card_body(Some("two\u{0}steps\u{7}found"), &request);
    assert_eq!(body, "twostepsfound");
    assert!(!body.contains('\u{0}'));
}

#[test]
fn promises_an_outbound_action_matches_words_not_substrings() {
    assert!(promises_an_outbound_action("I sent it"));
    assert!(promises_an_outbound_action("SENDING now"));
    // `resent` is screened, and the near-miss is not a mistake: the past tense
    // of "to resent" is "resented", so a bare `resent` in a card body means
    // re-sent. `senders`, `descendant` and `presented` are the real near
    // misses, and word-boundary matching is what keeps them out.
    assert!(promises_an_outbound_action("resent the invoice"));
    assert!(
        !promises_an_outbound_action("resented"),
        "a feeling, not an act"
    );
    assert!(
        !promises_an_outbound_action("senders"),
        "not a prefix match"
    );
    assert!(!promises_an_outbound_action("unsubscribe link"), "a noun");
    assert!(promises_an_outbound_action("unsubscribing you"), "an act");
    assert!(!promises_an_outbound_action("descendant"));
    assert!(!promises_an_outbound_action(""));
}

// -------------------------------------------------------------- the data --

#[test]
fn a_write_note_gate_produces_the_note_shape() {
    let request = gate(
        "write_note",
        json!({"title": "Kettle — next steps", "body_md": "…", "thread_id": "18f2a1b3c4d5e6f7"}),
    );
    let data = approval_data(&request, false);
    assert_eq!(data["action"], "write_note");
    assert_eq!(data["action_label"], "Save note");
    assert_eq!(data["note_title"], "Kettle — next steps");
    assert_eq!(data["note_id"], json!(Uuid::nil()));
    assert_eq!(data["thread_id"], "18f2a1b3c4d5e6f7");
    // Exactly `API.md` §7.1's key set. `validate.py`'s `OBJ` is exact, so an
    // extra key here is a contract violation the moment `/feed` serves it —
    // which is precisely how the two system cards' `reason` got in.
    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action",
            "action_label",
            "note_id",
            "note_title",
            "thread_id"
        ]
    );
}

#[test]
fn a_draft_reply_gate_produces_the_draft_shape() {
    let request = gate(
        "draft_reply",
        json!({
            "to": ["kamran@northbound.co"],
            "subject": "Re: Design review — Thursday",
            "body_text": "…",
            "thread_id": "18f28c5d6e7f8a9b"
        }),
    );
    let data = approval_data(&request, true);
    assert_eq!(data["action_label"], "Save draft");
    assert_eq!(data["draft_id"], json!(Uuid::nil()));
    assert_eq!(data["never_messaged"], json!(true));
    assert_eq!(data["to"], json!(["kamran@northbound.co"]));
    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action",
            "action_label",
            "draft_id",
            "never_messaged",
            "subject",
            "thread_id",
            "to"
        ]
    );
}

/// The tool's `thread_id` is optional and the card is raised **before** any
/// dispatch, so a model that omits it must still produce a contract-valid card.
/// `API.md` §7.1 was widened at P5 for exactly this.
#[test]
fn a_draft_reply_with_no_thread_still_cards() {
    let request = gate("draft_reply", json!({"to": ["a@b.com"], "subject": "Hi"}));
    let data = approval_data(&request, false);
    assert_eq!(data["thread_id"], json!(null));
    assert!(gate_thread_id(&request).is_none());
}

/// A sender-controlled string in `to` or `subject` lands in the card's `data`
/// and then in a `jsonb` column. A NUL there is not cosmetic: PostgreSQL
/// **rejects** one inside a `jsonb` string, and a live sync died of exactly
/// that (D29).
///
/// It is *not* neutralised, and that is the deliberate half. `fence::field`
/// de-fangs marker-shaped text for a string entering a prompt;
/// `fence::stored` does not, because this string is rendered to a person and
/// mangling a subject line would corrupt what they read for no gain.
#[test]
fn the_cards_stored_fields_are_scrubbed_but_not_mangled() {
    let subject = format!("Re: <<<{}-0123456789abcdef", fence::MARKER);
    let request = gate(
        "draft_reply",
        json!({ "to": ["ops@x.com\u{0}"], "subject": subject }),
    );
    let data = approval_data(&request, false);
    assert_eq!(data["to"][0], "ops@x.com", "the NUL is gone");
    assert_eq!(
        data["subject"], subject,
        "and the subject reaches the reader as it was written"
    );

    let title = gate("write_note", json!({"title": "Kettle\u{0}\u{7} — steps"}));
    assert_eq!(approval_data(&title, false)["note_title"], "Kettle — steps");
}

/// The cap is in bytes over a `String`, so an astral codepoint at the boundary
/// is where a naive slice panics.
#[test]
fn a_stored_field_caps_without_panicking_on_a_codepoint() {
    let request = gate(
        "draft_reply",
        json!({"to": ["a@b.com"], "subject": "🙂".repeat(2_000)}),
    );
    let data = approval_data(&request, false);
    assert!(data["subject"].as_str().unwrap().len() <= tools::MAX_SUBJECT_BYTES);
}

#[test]
fn an_unknown_gated_tool_falls_back_to_the_shape_that_promises_least() {
    let request = gate("some_future_tool", json!({}));
    let data = approval_data(&request, false);
    assert_eq!(data["action"], "write_note");
    assert_eq!(data["action_label"], "Save note");
}

// -------------------------------------------------------- the agent note --

#[test]
fn the_agent_note_names_the_agent_and_the_pending_action() {
    assert_eq!(
        agent_note("Job Search Tracker", "write_note"),
        "Job Search Tracker · a note to approve"
    );
    assert_eq!(
        agent_note("Reply Drafter", "draft_reply"),
        "Reply Drafter · a draft reply to approve"
    );
}

/// EDGE (unicode): the cap is in bytes and the name is the user's. Slicing on a
/// byte boundary inside a codepoint is a panic, and this string reaches a
/// 4 000-character path.
#[test]
fn the_agent_note_caps_without_panicking_on_a_codepoint() {
    let note = agent_note(&"🙂".repeat(1_000), "write_note");
    assert!(note.len() <= MAX_AGENT_NOTE, "{}", note.len());
    // `from_utf8` on a `String`'s bytes can never fail, so it asserted nothing.
    // What a byte cap can actually break is a codepoint, and `tools::cap`
    // appends its own truncation marker — so the property is that everything
    // *before* the marker is whole emoji and nothing else.
    let kept = note
        .split_once(fence::TRUNCATION_MARKER)
        .map_or(note.as_str(), |(head, _)| head);
    assert!(!kept.is_empty(), "{note}");
    assert!(
        kept.chars().all(|c| c == '🙂'),
        "a codepoint was split: {kept:?}"
    );
}

// ------------------------------------------------------- the info shapes --

/// The one author of every `action: "none"` payload, including the two the
/// system raises for itself. Both used to carry a fifth key, `reason`, which
/// `FEED_DATA`'s exact key set forbids — and nothing could notice while
/// `/feed` was unmounted.
#[test]
fn info_data_is_exactly_the_contracts_third_shape() {
    let data = info_data(None, None, None);
    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["action", "draft_id", "note_id", "thread_id"]);
    assert_eq!(data["action"], "none");
    assert_eq!(data["note_id"], json!(null));

    let data = info_data(
        Some(Uuid::nil()),
        Some(Uuid::nil()),
        Some("18f2a1b3c4d5e6f7"),
    );
    assert_eq!(data["note_id"], json!(Uuid::nil()));
    assert_eq!(data["draft_id"], json!(Uuid::nil()));
    assert_eq!(data["thread_id"], "18f2a1b3c4d5e6f7");
}

#[test]
fn the_ttl_is_the_contracts_seven_days() {
    assert_eq!(APPROVAL_TTL_DAYS, 7);
}
