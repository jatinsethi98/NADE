//! Fixture conformance: the **real response types**, serialised, compared
//! against `docs/contract/` as parsed `serde_json::Value` so key order cannot
//! matter.
//!
//! This is the seam that stops the two lanes drifting. The iOS side decodes the
//! same files; if a field is renamed, made nullable, or dropped here, this test
//! fails on the backend at the same moment the decode test fails on iOS.
//!
//! Where a fixture and `docs/API.md` disagree, **`API.md` wins** and the
//! discrepancy is reported rather than patched into `docs/`.

use serde_json::{json, Value};

use super::mail::{
    AgentCard, AttachmentView, Mailbox, MailboxesResponse, MeResponse, MessageView, ThreadDetail,
    ThreadSummary, ThreadsResponse,
};
use crate::test_support::fixture;

/// Serialise a real response type and compare it with a fixture.
fn assert_matches<T: serde::Serialize>(value: &T, fixture_name: &str) {
    let produced = serde_json::to_value(value).expect("our response types must serialise");
    let expected = fixture(fixture_name);
    assert_eq!(
        produced,
        expected,
        "\n{fixture_name} and our response type disagree.\n\
         produced: {}\nexpected: {}\n",
        serde_json::to_string_pretty(&produced).unwrap_or_default(),
        serde_json::to_string_pretty(&expected).unwrap_or_default()
    );
}

/// Every key the fixture has, our type has - and vice versa. Compared as a
/// structure so a value difference (a count, a timestamp) is not confused with
/// a *shape* difference.
fn assert_same_shape(produced: &Value, expected: &Value, path: &str) {
    match (produced, expected) {
        (Value::Object(ours), Value::Object(theirs)) => {
            let mut our_keys: Vec<&String> = ours.keys().collect();
            let mut their_keys: Vec<&String> = theirs.keys().collect();
            our_keys.sort();
            their_keys.sort();
            assert_eq!(our_keys, their_keys, "different fields at {path}");
            for (key, value) in ours {
                assert_same_shape(value, &theirs[key], &format!("{path}.{key}"));
            }
        }
        (Value::Array(ours), Value::Array(theirs)) => {
            // Compare the first element's shape; an empty fixture array carries
            // no shape to check, which is why every endpoint also has a
            // populated fixture.
            if let (Some(ours), Some(theirs)) = (ours.first(), theirs.first()) {
                assert_same_shape(ours, theirs, &format!("{path}[0]"));
            }
        }
        (Value::Null, _) | (_, Value::Null) => {
            // A nullable field: the contract marks it `|null`, so either side
            // being null here is legal and the type is checked by the other
            // fixture of the pair.
        }
        (ours, theirs) => assert_eq!(
            std::mem::discriminant(ours),
            std::mem::discriminant(theirs),
            "different type at {path}: {ours} vs {theirs}"
        ),
    }
}

fn shape_of<T: serde::Serialize>(value: &T, fixture_name: &str) {
    let produced = serde_json::to_value(value).expect("our response types must serialise");
    assert_same_shape(&produced, &fixture(fixture_name), fixture_name);
}

// -------------------------------------------------------------- /me --

#[test]
fn me_matches_both_fixtures() {
    assert_matches(
        &MeResponse {
            email: "jatinsethi98@gmail.com".to_owned(),
            status: "ok".to_owned(),
        },
        "me.json",
    );
    assert_matches(
        &MeResponse {
            email: "jatinsethi98@gmail.com".to_owned(),
            status: "needs_reauth".to_owned(),
        },
        "me_needs_reauth.json",
    );
}

// -------------------------------------------------------- /mailboxes --

#[test]
fn mailboxes_match_the_fixture_exactly() {
    let fixture_value = fixture("mailboxes.json");
    let mailboxes: Vec<Mailbox> = fixture_value["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| Mailbox {
            id: entry["id"].as_str().unwrap().to_owned(),
            name: entry["name"].as_str().unwrap().to_owned(),
            kind: entry["kind"].as_str().unwrap().to_owned(),
            unread: entry["unread"].as_i64().unwrap(),
            total: entry["total"].as_i64().unwrap(),
        })
        .collect();

    assert_matches(&MailboxesResponse { mailboxes }, "mailboxes.json");

    // And the whitelist the fixture uses is the one the code enforces, in the
    // same order - the part a hand-built list would drift on.
    let system: Vec<(&str, &str)> = fixture_value["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "system")
        .map(|entry| {
            (
                entry["id"].as_str().unwrap(),
                entry["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        system.as_slice(),
        super::mail::SYSTEM_MAILBOXES.as_slice(),
        "the eight system mailboxes and their display names are API.md §2"
    );

    // Not paginated: no cursor field at all.
    let produced = serde_json::to_value(MailboxesResponse {
        mailboxes: Vec::new(),
    })
    .unwrap();
    assert_eq!(produced.as_object().unwrap().len(), 1);
    assert!(produced.get("next_cursor").is_none());
}

// ----------------------------------------------------------- threads --

fn thread_summary(entry: &Value) -> ThreadSummary {
    ThreadSummary {
        id: entry["id"].as_str().unwrap().to_owned(),
        subject: entry["subject"].as_str().unwrap().to_owned(),
        snippet: entry["snippet"].as_str().unwrap().to_owned(),
        from_name: entry["from_name"].as_str().unwrap().to_owned(),
        from_email: entry["from_email"].as_str().unwrap().to_owned(),
        ts: entry["ts"].as_str().unwrap().to_owned(),
        unread: entry["unread"].as_bool().unwrap(),
        msg_count: i32::try_from(entry["msg_count"].as_i64().unwrap()).unwrap(),
        agent_note: entry["agent_note"].as_str().map(str::to_owned),
    }
}

fn threads_response(name: &str) -> ThreadsResponse {
    let value = fixture(name);
    ThreadsResponse {
        threads: value["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(thread_summary)
            .collect(),
        next_cursor: value["next_cursor"].as_str().map(str::to_owned),
    }
}

#[test]
fn all_three_thread_page_shapes_match() {
    for name in [
        "threads.json",
        "threads_last_page.json",
        "threads_empty.json",
    ] {
        assert_matches(&threads_response(name), name);
    }
}

#[test]
fn both_search_page_shapes_match() {
    for name in ["search.json", "search_empty.json"] {
        assert_matches(&threads_response(name), name);
    }
}

/// The empty case is where clients crash: an empty array **and**
/// `next_cursor: null`, never a 404 and never a missing field.
#[test]
fn an_empty_page_still_carries_next_cursor() {
    let produced = serde_json::to_value(ThreadsResponse {
        threads: Vec::new(),
        next_cursor: None,
    })
    .unwrap();
    assert_eq!(produced, json!({"threads": [], "next_cursor": null}));
    assert!(
        produced.as_object().unwrap().contains_key("next_cursor"),
        "the field is always present, even when null"
    );
}

/// The cursor in the fixture is one of ours, and decodes to the last row on the
/// page. That is what makes the fixture a real example rather than a plausible
/// string.
#[test]
fn the_fixture_cursor_is_a_real_keyset_cursor() {
    let value = fixture("threads.json");
    let cursor = value["next_cursor"].as_str().expect("a populated page");
    let keyset = super::cursor::decode(cursor).expect("the fixture cursor must be one of ours");

    let last = value["threads"].as_array().unwrap().last().unwrap();
    assert_eq!(keyset.id, last["id"].as_str().unwrap());
    assert_eq!(
        super::mail::wire_ts(keyset.ts),
        last["ts"].as_str().unwrap(),
        "the cursor is the ts+id of the last row on the page"
    );

    // And re-encoding it is byte-identical, so the two sides agree on the form.
    assert_eq!(super::cursor::encode(keyset.ts, &keyset.id), cursor);
}

// ------------------------------------------------------ thread detail --

fn thread_detail(name: &str) -> ThreadDetail {
    let value = fixture(name);
    ThreadDetail {
        id: value["id"].as_str().unwrap().to_owned(),
        subject: value["subject"].as_str().unwrap().to_owned(),
        mailbox_name: value["mailbox_name"].as_str().unwrap().to_owned(),
        account_email: value["account_email"].as_str().unwrap().to_owned(),
        messages: value["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| MessageView {
                gmail_id: message["gmail_id"].as_str().unwrap().to_owned(),
                from_name: message["from_name"].as_str().unwrap().to_owned(),
                from_email: message["from_email"].as_str().unwrap().to_owned(),
                to: strings(&message["to"]),
                cc: strings(&message["cc"]),
                ts: message["ts"].as_str().unwrap().to_owned(),
                body_text: message["body_text"].as_str().unwrap().to_owned(),
                body_html: message["body_html"].as_str().map(str::to_owned),
                label_ids: strings(&message["label_ids"]),
                attachments: message["attachments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|attachment| AttachmentView {
                        id: attachment["id"].as_str().unwrap().to_owned(),
                        name: attachment["name"].as_str().unwrap().to_owned(),
                        mime: attachment["mime"].as_str().unwrap().to_owned(),
                        size: attachment["size"].as_i64().unwrap(),
                        inline: attachment["inline"].as_bool().unwrap(),
                    })
                    .collect(),
            })
            .collect(),
        agent_cards: value["agent_cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| AgentCard {
                run_id: card["run_id"].as_str().unwrap().to_owned(),
                agent_name: card["agent_name"].as_str().unwrap().to_owned(),
                status: card["status"].as_str().unwrap().to_owned(),
                summary: card["summary"].as_str().unwrap().to_owned(),
                feed_item_id: card["feed_item_id"].as_str().map(str::to_owned),
            })
            .collect(),
        partial: value["partial"].as_bool().unwrap(),
    }
}

#[test]
fn every_thread_detail_fixture_matches() {
    for name in [
        "thread.json",
        "thread_html_only.json",
        "thread_partial.json",
    ] {
        assert_matches(&thread_detail(name), name);
    }
}

/// `thread_partial.json` is the only fixture where `partial` is true, and it
/// exists because `API.md` §2 has always said clients must surface that state
/// while nothing on either lane had ever serialised it. The shape has to
/// round-trip through our real response type like any other — otherwise the
/// case the iOS lane renders is one this server has never produced.
#[test]
fn the_partial_fixture_is_partial_and_shorter_than_its_row() {
    let value = fixture("thread_partial.json");
    assert!(
        value["partial"].as_bool().unwrap(),
        "thread_partial.json is the partial-thread fixture and must say so"
    );

    // The point of the fixture: the detail is a *subset*. A client that derives
    // one number from the other gets this thread wrong.
    let detail_messages = value["messages"].as_array().unwrap().len();
    let row = fixture("search.json")["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == value["id"])
        .expect("the partial thread reached the cache through a search hit")
        .clone();
    let counted = row["msg_count"].as_u64().unwrap() as usize;
    assert!(
        detail_messages < counted,
        "a partial detail must carry fewer messages ({detail_messages}) than the row counts ({counted})"
    );
}

/// `API.md` §2's two hard rules about a message, checked against the fixtures
/// rather than against our own opinion.
#[test]
fn a_message_has_no_id_and_body_html_is_genuinely_nullable() {
    for name in [
        "thread.json",
        "thread_html_only.json",
        "thread_partial.json",
    ] {
        for message in fixture(name)["messages"].as_array().unwrap() {
            assert!(
                message.get("id").is_none(),
                "{name}: a message must carry gmail_id and no `id`"
            );
            assert!(
                !message["body_text"].is_null(),
                "{name}: body_text is never null"
            );
        }
    }

    // One fixture must actually exercise the null, or the iOS optional is
    // never tested.
    let nulls = fixture("thread.json")["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["body_html"].is_null())
        .count();
    assert!(
        nulls > 0,
        "thread.json must include a message with no text/html part"
    );

    // `cid:` is rewritten before storage, so it can never appear on the wire.
    for name in ["thread.json", "thread_html_only.json"] {
        for message in fixture(name)["messages"].as_array().unwrap() {
            if let Some(html) = message["body_html"].as_str() {
                assert!(
                    !html.to_ascii_lowercase().contains("cid:"),
                    "{name}: cid: is rewritten at parse time, never at response time"
                );
            }
        }
    }
}

/// Every `cid:`-rewritten URL in a fixture points at an attachment that fixture
/// declares - the referential integrity the proxy depends on.
#[test]
fn rewritten_inline_images_point_at_a_declared_attachment() {
    for name in ["thread.json", "thread_html_only.json"] {
        let value = fixture(name);
        for message in value["messages"].as_array().unwrap() {
            let Some(html) = message["body_html"].as_str() else {
                continue;
            };
            let gmail_id = message["gmail_id"].as_str().unwrap();
            let prefix = format!("/v1/messages/{gmail_id}/attachments/");
            let declared: Vec<&str> = message["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|attachment| attachment["id"].as_str().unwrap())
                .collect();

            let mut rest = html;
            while let Some(at) = rest.find(&prefix) {
                let tail = &rest[at + prefix.len()..];
                let end = tail.find(['"', '\'', ' ', '>']).unwrap_or(tail.len());
                let att_id = &tail[..end];
                assert!(
                    declared.contains(&att_id),
                    "{name}: {att_id} is referenced by the HTML but not declared as an attachment"
                );
                let inline = message["attachments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|attachment| attachment["id"] == att_id)
                    .unwrap()["inline"]
                    .as_bool()
                    .unwrap();
                assert!(inline, "{name}: a cid-referenced part must be inline: true");
                rest = &tail[end..];
            }
        }
    }
}

// ------------------------------------------------------------ health --

#[test]
fn healthz_matches_both_fixtures() {
    // The health handler builds its body inline, so this pins the shape the
    // contract expects against the crate version the handler reports.
    assert_eq!(
        fixture("healthz.json"),
        json!({"status": "ok", "db": "ok", "version": crate::VERSION})
    );
    assert_eq!(
        fixture("healthz_db_down.json"),
        json!({"status": "degraded", "db": "down", "version": crate::VERSION})
    );
}

// -------------------------------------------------------- shape only --

/// Endpoints P2 does not serve yet still have to keep their fixtures parseable
/// and self-consistent, because the iOS lane decodes them now. This is the
/// cheap half of the guarantee; P4-P7 replace it with real types.
#[test]
fn fixtures_for_later_phases_are_well_formed() {
    for name in [
        "agents.json",
        "agents_empty.json",
        "agent.json",
        "agent_compile_failed.json",
        "runs.json",
        "run.json",
        "run_done.json",
        "run_failed.json",
        "feed.json",
        "feed_empty.json",
        "feed_item.json",
        "feed_item_info.json",
        "notes.json",
        "notes_empty.json",
        "note.json",
        "drafts.json",
        "drafts_empty.json",
        "draft.json",
        "settings.json",
        "approve.json",
        "skip.json",
        "seen.json",
        "pair.json",
    ] {
        let value = fixture(name);
        assert!(
            value.is_object(),
            "{name} should be an object at the top level"
        );
    }

    // The paginated ones all carry `next_cursor`, present and possibly null.
    for name in [
        "notes.json",
        "notes_empty.json",
        "drafts.json",
        "drafts_empty.json",
        "feed.json",
        "feed_empty.json",
        "runs.json",
        "runs_empty.json",
    ] {
        let value = fixture(name);
        assert!(
            value.as_object().unwrap().contains_key("next_cursor"),
            "{name} is paginated, so next_cursor is always present"
        );
    }

    // And the bounded ones never invent one.
    for name in [
        "mailboxes.json",
        "agents.json",
        "agents_empty.json",
        "settings.json",
    ] {
        let value = fixture(name);
        assert!(
            !value.as_object().unwrap().contains_key("next_cursor"),
            "{name} is not paginated; a cursor field implies a page boundary that \
             will never exist"
        );
    }
}

/// The shape helper itself must not pass two different shapes.
#[test]
fn the_shape_comparison_actually_compares() {
    shape_of(
        &MeResponse {
            email: "x".to_owned(),
            status: "ok".to_owned(),
        },
        "me.json",
    );

    let outcome = std::panic::catch_unwind(|| {
        assert_same_shape(&json!({"a": 1}), &json!({"b": 1}), "root");
    });
    assert!(outcome.is_err(), "a missing field must fail the comparison");

    let outcome = std::panic::catch_unwind(|| {
        assert_same_shape(&json!({"a": "1"}), &json!({"a": 1}), "root");
    });
    assert!(outcome.is_err(), "a changed type must fail the comparison");
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().to_owned())
        .collect()
}

// ------------------------------------------------------- P4: the agents --

use super::agents::{AgentDetail, AgentSummary, AgentsResponse};
use super::drafts::{Draft, DraftsResponse};
use super::notes::{NoteDetail, NoteSummary, NotesResponse};
use super::runs::{JournalEntry, RunDetail, RunSummary, RunsResponse};

fn an_agent_summary() -> AgentSummary {
    AgentSummary {
        id: uuid::Uuid::nil(),
        name: "Job Search Tracker".to_owned(),
        nl_definition: "When a recruiter emails, save a note.".to_owned(),
        status: "published".to_owned(),
        trigger_summary: "On new mail".to_owned(),
        schedule: None,
        last_run_at: Some("2026-08-16T09:13:41Z".to_owned()),
        approval_required: true,
    }
}

#[test]
fn the_agent_list_row_matches_the_fixture_shape() {
    shape_of(
        &AgentsResponse {
            agents: vec![an_agent_summary()],
        },
        "agents.json",
    );
}

#[test]
fn the_full_agent_matches_the_fixture_shape() {
    // `#[serde(flatten)]` on the summary is the risk here: a flattened struct
    // that stopped flattening would nest the list fields under a key and every
    // client would break. Comparing against the fixture is what catches it.
    shape_of(
        &AgentDetail {
            summary: an_agent_summary(),
            allowed_tools: vec!["search_mail".to_owned()],
            when_span: Some("a recruiter emails".to_owned()),
            do_span: Some("save a note".to_owned()),
            trailing: Some("Ask me first.".to_owned()),
            compile_error: None,
            spec: Some(fixture("agent.json")["spec"].clone()),
        },
        "agent.json",
    );
}

#[test]
fn an_empty_agent_list_matches_and_carries_no_cursor() {
    assert_matches(&AgentsResponse { agents: Vec::new() }, "agents_empty.json");
}

#[test]
fn the_full_agent_carries_every_list_field_as_well() {
    // The flatten, from the other direction: `GET /agents/{id}` is a superset of
    // a list row, so a client that only knows the list shape still decodes it.
    let detail = serde_json::to_value(AgentDetail {
        summary: an_agent_summary(),
        allowed_tools: Vec::new(),
        when_span: None,
        do_span: None,
        trailing: None,
        compile_error: None,
        spec: None,
    })
    .unwrap();
    let summary = serde_json::to_value(an_agent_summary()).unwrap();
    for key in summary.as_object().unwrap().keys() {
        assert!(detail.get(key).is_some(), "the full agent is missing {key}");
    }
}

// --------------------------------------------------------- P4: the runs --

#[test]
fn the_run_list_row_matches_the_fixture_shape() {
    shape_of(
        &RunsResponse {
            runs: vec![RunSummary {
                id: uuid::Uuid::nil(),
                agent_id: uuid::Uuid::nil(),
                agent_name: "Job Search Tracker".to_owned(),
                trigger_kind: "mail".to_owned(),
                status: "done".to_owned(),
                summary: Some("Two next steps found".to_owned()),
                created_at: "2026-08-16T09:13:41Z".to_owned(),
                updated_at: "2026-08-16T09:13:58Z".to_owned(),
            }],
            next_cursor: None,
        },
        "runs.json",
    );
}

#[test]
fn the_run_detail_matches_the_fixture_shape() {
    let fixture_run = fixture("run.json");
    let journal: Vec<JournalEntry> = fixture_run["journal"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| JournalEntry {
            seq: i32::try_from(entry["seq"].as_i64().unwrap()).unwrap(),
            kind: entry["kind"].as_str().unwrap().to_owned(),
            payload: entry["payload"].clone(),
            created_at: entry["created_at"].as_str().unwrap().to_owned(),
        })
        .collect();

    // The journal is served **verbatim** (`API.md` §6.1), so this is not just a
    // shape check: re-serialising the fixture's own entries must reproduce them
    // byte for byte, which is what "verbatim" means in practice.
    let produced = serde_json::to_value(&journal).unwrap();
    assert_eq!(
        produced, fixture_run["journal"],
        "the journal was not served verbatim"
    );

    shape_of(
        &RunDetail {
            id: uuid::Uuid::nil(),
            agent_id: uuid::Uuid::nil(),
            agent_name: "Job Search Tracker".to_owned(),
            status: "pending_approval".to_owned(),
            trigger_kind: "mail".to_owned(),
            trigger_ref: Some("18f2c4d5e6f70819".to_owned()),
            error: None,
            journal,
        },
        "run.json",
    );
}

#[test]
fn every_run_fixture_serves_its_journal_verbatim() {
    // All six run fixtures, not only the one above: each exercises a different
    // terminal shape, and the verbatim rule holds for every one.
    for name in [
        "run.json",
        "run_done.json",
        "run_failed.json",
        "run_pending_draft.json",
        "run_skipped.json",
        "run_expired.json",
    ] {
        let body = fixture(name);
        let journal: Vec<JournalEntry> = body["journal"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| JournalEntry {
                seq: i32::try_from(entry["seq"].as_i64().unwrap()).unwrap(),
                kind: entry["kind"].as_str().unwrap().to_owned(),
                payload: entry["payload"].clone(),
                created_at: entry["created_at"].as_str().unwrap().to_owned(),
            })
            .collect();
        assert_eq!(
            serde_json::to_value(&journal).unwrap(),
            body["journal"],
            "{name}"
        );
    }
}

#[test]
fn an_empty_run_log_matches() {
    assert_matches(
        &RunsResponse {
            runs: Vec::new(),
            next_cursor: None,
        },
        "runs_empty.json",
    );
}

// ---------------------------------------------- P4: the notes and drafts --

#[test]
fn the_note_list_row_matches_the_fixture_shape() {
    shape_of(
        &NotesResponse {
            notes: vec![NoteSummary {
                id: uuid::Uuid::nil(),
                run_id: Some(uuid::Uuid::nil()),
                thread_id: Some("18f2a1b3c4d5e6f7".to_owned()),
                title: "Kettle - next steps".to_owned(),
                agent_name: Some("Job Search Tracker".to_owned()),
                unread: true,
                updated_at: "2026-08-16T09:13:58Z".to_owned(),
            }],
            next_cursor: None,
        },
        "notes.json",
    );
}

#[test]
fn the_note_detail_matches_the_fixture_shape() {
    shape_of(
        &NoteDetail {
            id: uuid::Uuid::nil(),
            title: "Kettle - next steps".to_owned(),
            body_md: "# Kettle".to_owned(),
            run_id: Some(uuid::Uuid::nil()),
            thread_id: Some("18f2a1b3c4d5e6f7".to_owned()),
            agent_name: Some("Job Search Tracker".to_owned()),
            unread: false,
            updated_at: "2026-08-16T09:13:58Z".to_owned(),
        },
        "note.json",
    );
}

#[test]
fn the_note_detail_fixture_reports_the_state_after_the_read() {
    // `API.md` §3: reading marks it read, so the detail is always `false`.
    assert_eq!(fixture("note.json")["unread"], json!(false));
}

#[test]
fn the_draft_matches_the_fixture_shape() {
    shape_of(
        &DraftsResponse {
            drafts: vec![Draft {
                id: uuid::Uuid::nil(),
                run_id: Some(uuid::Uuid::nil()),
                thread_id: Some("18f2a1b3c4d5e6f7".to_owned()),
                to: vec!["priya@kettle.com".to_owned()],
                subject: "Re: Staff Product Designer".to_owned(),
                body_text: "Hi Priya".to_owned(),
                created_at: "2026-08-16T09:13:41Z".to_owned(),
                updated_at: "2026-08-17T07:41:12Z".to_owned(),
            }],
            next_cursor: None,
        },
        "drafts.json",
    );
}

#[test]
fn the_patch_response_is_the_same_shape_as_a_list_row() {
    // `API.md` §11: there is deliberately no `GET /drafts/{id}` because the list
    // carries the whole draft and `PATCH` returns it - which is only true if the
    // two shapes are identical.
    shape_of(
        &Draft {
            id: uuid::Uuid::nil(),
            run_id: Some(uuid::Uuid::nil()),
            thread_id: Some("18f2a1b3c4d5e6f7".to_owned()),
            to: vec!["priya@kettle.com".to_owned()],
            subject: "Re: Staff Product Designer".to_owned(),
            body_text: "Hi Priya".to_owned(),
            created_at: "2026-08-16T09:13:41Z".to_owned(),
            updated_at: "2026-08-17T07:41:12Z".to_owned(),
        },
        "draft.json",
    );
}

#[test]
fn the_empty_note_and_draft_pages_match() {
    assert_matches(
        &NotesResponse {
            notes: Vec::new(),
            next_cursor: None,
        },
        "notes_empty.json",
    );
    assert_matches(
        &DraftsResponse {
            drafts: Vec::new(),
            next_cursor: None,
        },
        "drafts_empty.json",
    );
}

// --------------------------------------------------- fixture completeness --

/// Every fixture is referenced by some test, or is explicitly deferred.
///
/// The failure this prevents is a fixture that exists, is generated, is checked
/// by `validate.py` for *internal* consistency — and is served by nothing,
/// decoded by nothing, and matched against no real type. That file looks like
/// coverage and is not.
///
/// "Referenced" is checked by scanning this crate's sources for the file name.
/// Crude, and right for the job: a test that names a fixture is using it, and a
/// fixture no source mentions is by definition unserved.
#[test]
fn every_fixture_is_referenced_by_a_test_or_deferred_on_purpose() {
    /// Fixtures no P4 code can serve yet, each with the phase that will.
    const DEFERRED: &[(&str, &str)] = &[
        // P5 — the approval loop. `error.rs` has eight codes; `API.md` has
        // thirteen, and these five are the approve/skip transaction's. They are
        // listed here rather than added as dead `ErrorCode` variants, which
        // would make this test pass by making the code worse.
        ("error_conflict.json", "P5: the approve transaction's 409"),
        ("error_token_consumed.json", "P5: a replayed approval"),
        ("error_gone.json", "P5: an expired resource"),
        ("error_approval_expired.json", "P5: the 7-day sweep"),
        (
            "error_forbidden.json",
            "P5+: reserved; v1 is single-account",
        ),
        ("feed.json", "P5: the feed producer"),
        ("feed_empty.json", "P5"),
        ("feed_item.json", "P5"),
        ("feed_item_editable.json", "P5"),
        ("feed_item_info.json", "P5"),
        ("approve.json", "P5: POST /feed/{id}/approve"),
        ("skip.json", "P5: POST /feed/{id}/skip"),
        ("seen.json", "P5: POST /feed/seen"),
        // P6 — Ask. Request bodies and SSE streams, neither of which is a
        // response type this crate serialises.
        ("ask_request.json", "P6: a request body, not a response"),
        ("ask_request_route_hint.json", "P6: a request body"),
        ("ask_answer.sse", "P6: an SSE stream"),
        ("ask_results.sse", "P6: an SSE stream"),
        ("ask_agent_draft.sse", "P6: an SSE stream"),
        ("ask_error.sse", "P6: an SSE stream"),
        // P7 — settings.
        ("settings.json", "P7: GET/PATCH /settings"),
    ];

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = read_sources(&root.join("src"));

    let mut unreferenced = Vec::new();
    for entry in std::fs::read_dir(crate::test_support::contract_dir()).expect("docs/contract") {
        let path = entry.expect("dir entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.ends_with(".json") || name.ends_with(".sse")) {
            continue;
        }
        if DEFERRED.iter().any(|(deferred, _)| *deferred == name) {
            continue;
        }
        // `error::tests::every_served_code_matches_its_contract_fixture` builds
        // its filenames with `format!`, so a scan for literals cannot see them.
        // Recognising the pattern is exact rather than a fudge: a fixture named
        // for a code the server actually serves *is* checked, byte for byte,
        // and one named for a code with no `ErrorCode` variant is not - which
        // is precisely the five P5 owes.
        if is_a_served_error_fixture(name) {
            continue;
        }
        if !sources.contains(name) {
            unreferenced.push(name.to_owned());
        }
    }
    unreferenced.sort();

    assert!(
        unreferenced.is_empty(),
        "these fixtures are served and checked by nothing:\n  {}\n\
         Either add a test that uses them, or add them to DEFERRED with the \
         phase that will.",
        unreferenced.join("\n  ")
    );
}

/// Whether `name` is `error_<code>.json` for a code this server serves.
fn is_a_served_error_fixture(name: &str) -> bool {
    use crate::error::ErrorCode;
    const SERVED: &[ErrorCode] = &[
        ErrorCode::BadRequest,
        ErrorCode::Unauthorized,
        ErrorCode::NotFound,
        ErrorCode::PayloadTooLarge,
        ErrorCode::RateLimited,
        ErrorCode::NeedsReauth,
        ErrorCode::UpstreamUnavailable,
        ErrorCode::Internal,
    ];
    SERVED
        .iter()
        .any(|code| name == format!("error_{}.json", code.as_str()))
}

/// Every `.rs` file under `dir`, concatenated.
fn read_sources(dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                }
            }
        }
    }
    out
}

#[test]
fn the_deferred_list_names_only_fixtures_that_exist() {
    // A stale entry would silently excuse nothing, and would hide the day the
    // file it names is finally served.
    let dir = crate::test_support::contract_dir();
    let present: Vec<String> = std::fs::read_dir(&dir)
        .expect("docs/contract")
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();
    for name in [
        "error_conflict.json",
        "feed.json",
        "ask_answer.sse",
        "settings.json",
    ] {
        assert!(
            present.iter().any(|p| p == name),
            "{name} is deferred but gone"
        );
    }
}

// ------------------------------------------- the agent fixtures' invariants --

#[test]
fn every_agent_fixture_holds_the_spec_and_span_invariants() {
    // The four rules `validate.py` enforces on the corpus, restated against the
    // fixtures the server has to be able to produce. `POST /agents` and
    // `PATCH /agents/{id}` are written to keep exactly these.
    for name in [
        "agent.json",
        "agent_draft.json",
        "agent_scheduled.json",
        "agent_compile_failed.json",
    ] {
        let agent = fixture(name);
        let spec_null = agent["spec"].is_null();

        // spec null XOR compile_error set.
        assert_eq!(
            spec_null,
            !agent["compile_error"].is_null(),
            "{name}: exactly one of spec and compile_error is how a compile failure is recorded"
        );

        if spec_null {
            // A failed compile keeps the sentence and nothing derived from it.
            for field in ["when_span", "do_span", "trailing"] {
                assert!(agent[field].is_null(), "{name}: {field} outlived its spec");
            }
            assert_eq!(
                agent["status"],
                json!("draft"),
                "{name}: it must still be a draft"
            );
        } else {
            // The builder renders "When {when_span}, {do_span}." and cannot
            // without both. `trailing` may legitimately be null.
            for field in ["when_span", "do_span"] {
                assert!(
                    !agent[field].is_null(),
                    "{name}: {field} is needed to render"
                );
            }
            // spec.tools is a subset of allowed_tools.
            let allowed: Vec<&str> = agent["allowed_tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap())
                .collect();
            for tool in agent["spec"]["tools"].as_array().unwrap() {
                let tool = tool.as_str().unwrap();
                assert!(allowed.contains(&tool), "{name}: {tool} is not allowed");
                assert!(
                    crate::agents::tools::V1_TOOLS.contains(&tool),
                    "{name}: {tool} is not a v1 tool"
                );
            }
            // A schedule trigger and a schedule imply each other.
            assert_eq!(
                agent["spec"]["trigger"]["kind"] == json!("schedule"),
                !agent["schedule"].is_null(),
                "{name}: the trigger kind and the schedule disagree"
            );
        }
        assert!(
            !agent["nl_definition"].as_str().unwrap().is_empty(),
            "{name}"
        );
    }
}
