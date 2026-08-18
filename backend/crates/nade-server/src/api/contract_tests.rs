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
fn both_thread_detail_fixtures_match() {
    for name in ["thread.json", "thread_html_only.json"] {
        assert_matches(&thread_detail(name), name);
    }
}

/// `API.md` §2's two hard rules about a message, checked against the fixtures
/// rather than against our own opinion.
#[test]
fn a_message_has_no_id_and_body_html_is_genuinely_nullable() {
    for name in ["thread.json", "thread_html_only.json"] {
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
