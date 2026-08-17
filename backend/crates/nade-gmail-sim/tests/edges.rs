//! The edge-case checklist from `CRITERIA.md`, at the API surface.
//!
//! Unit tests in the crate cover the pieces; this file covers the seams — what
//! actually comes back over the wire when the mailbox is empty, when a page
//! boundary falls exactly on the last row, when a batch is a hundred calls long,
//! and when the client's cursor is from the future.

use std::{sync::Arc, time::Duration};

use nade_gmail_sim::{
    api::{Method, SimRequest},
    batch::BatchOrder,
    error::ApiError,
    fault::{cost, Fault, FaultRule, OverQuota},
    history::HistoryRetention,
    message::ReceivedAt,
    simulator::{AuthMode, Config},
    MessageSpec, Simulator,
};
use serde_json::Value;

const DAY_MS: i64 = 86_400_000;

fn sim() -> Simulator {
    Simulator::new()
}

fn get(sim: &Simulator, target: &str) -> nade_gmail_sim::api::SimResponse {
    sim.handle(&sim.authorized(target))
}

fn seed(sim: &Simulator, count: usize) -> Vec<String> {
    // One hour apart, newest last, so listing order is unambiguous.
    (0..count)
        .map(|n| {
            let at = sim.clock().now_ms() - (count - n) as i64 * 3_600_000;
            sim.insert_message(
                &MessageSpec::new()
                    .subject(format!("m{n}"))
                    .text(format!("body {n}"))
                    .received_at(ReceivedAt::AtMs(at)),
            )
            .unwrap()
        })
        .collect()
}

// -- E1: empty mailbox -------------------------------------------------------

#[test]
fn empty_mailbox_lists_nothing_and_says_so_the_way_gmail_does() {
    let sim = sim();
    let listed = get(&sim, "/gmail/v1/users/me/messages").json_body();
    assert!(
        listed.get("messages").is_none(),
        "Gmail omits the key entirely: {listed}"
    );
    assert!(listed.get("nextPageToken").is_none());
    assert_eq!(listed["resultSizeEstimate"], 0);

    let profile = get(&sim, "/gmail/v1/users/me/profile").json_body();
    assert_eq!(profile["messagesTotal"], 0);
    assert_eq!(profile["threadsTotal"], 0);
    assert!(profile["historyId"].is_string());

    let history = get(
        &sim,
        &format!(
            "/gmail/v1/users/me/history?startHistoryId={}",
            sim.history_id()
        ),
    )
    .json_body();
    assert!(history.get("history").is_none());
    assert_eq!(history["historyId"], sim.history_id().to_string());

    // Labels are never empty: a Gmail mailbox always has its system labels.
    let labels = get(&sim, "/gmail/v1/users/me/labels").json_body();
    assert_eq!(labels["labels"].as_array().unwrap().len(), 14);
}

// -- E2: a single message ----------------------------------------------------

#[test]
fn single_message_renders_in_every_format() {
    let sim = sim();
    let id = sim
        .insert_message(&MessageSpec::new().subject("only").text("just this"))
        .unwrap();

    let listed = get(&sim, "/gmail/v1/users/me/messages").json_body();
    let rows = listed["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["threadId"], id, "a first message threads on itself");
    assert_eq!(
        rows[0].as_object().unwrap().len(),
        2,
        "a list entry is ids only"
    );

    for format in ["minimal", "metadata", "full", "raw"] {
        let body = get(
            &sim,
            &format!("/gmail/v1/users/me/messages/{id}?format={format}"),
        )
        .json_body();
        assert_eq!(body["id"], id, "{format}");
        assert!(
            body["snippet"].as_str().unwrap().contains("just this"),
            "{format}"
        );
    }
}

// -- E3, E4, E16, E17, E18: paging -------------------------------------------

#[test]
fn pagination_on_the_exact_boundary() {
    let sim = sim();
    seed(&sim, 5);

    // maxResults == the whole result set: no token.
    let exact = get(&sim, "/gmail/v1/users/me/messages?maxResults=5").json_body();
    assert_eq!(exact["messages"].as_array().unwrap().len(), 5);
    assert!(
        exact.get("nextPageToken").is_none(),
        "a full page that exhausts the set must not offer another: {exact}"
    );
    // The estimate is NOT the count. Measured: 85 rows came back reported as
    // 201, pegged at 201 for every query. A client that sizes a progress bar or
    // an allocation from this field is wrong.
    assert_ne!(
        exact["resultSizeEstimate"].as_u64(),
        Some(5),
        "resultSizeEstimate must not be usable as a count"
    );

    // One short: a token, and page two holds exactly the last row.
    let short = get(&sim, "/gmail/v1/users/me/messages?maxResults=4").json_body();
    assert_eq!(short["messages"].as_array().unwrap().len(), 4);
    let token = short["nextPageToken"].as_str().expect("a token");

    let page_two = get(
        &sim,
        &format!("/gmail/v1/users/me/messages?maxResults=4&pageToken={token}"),
    )
    .json_body();
    assert_eq!(page_two["messages"].as_array().unwrap().len(), 1);
    assert!(page_two.get("nextPageToken").is_none());
    // …and it does not shrink to match the last page either, so it cannot even
    // be used to tell how much is left.
    assert_ne!(page_two["resultSizeEstimate"].as_u64(), Some(1));

    // The two pages together are the same five rows, in the same order, with no
    // row seen twice and none missing.
    let ids_of = |body: &Value| -> Vec<String> {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let mut paged = ids_of(&short);
    paged.extend(ids_of(&page_two));
    assert_eq!(paged, ids_of(&exact));
}

#[test]
fn message_deleted_between_pages_skips_nothing_and_repeats_nothing() {
    let sim = sim();
    let ids = seed(&sim, 6);

    let page_one = get(&sim, "/gmail/v1/users/me/messages?maxResults=3").json_body();
    let first: Vec<String> = page_one["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();
    let token = page_one["nextPageToken"].as_str().unwrap().to_owned();

    // The row the cursor *names* disappears before page two is asked for. This
    // is the case an offset cursor gets wrong: it would shift the whole listing
    // up by one and page two would skip a message forever.
    let cursor_row = first.last().unwrap().clone();
    sim.delete_message(&cursor_row).unwrap();

    let page_two = get(
        &sim,
        &format!("/gmail/v1/users/me/messages?maxResults=3&pageToken={token}"),
    )
    .json_body();
    let second: Vec<String> = page_two["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect();

    let mut seen = first.clone();
    seen.extend(second.clone());
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        seen.len(),
        unique.len(),
        "no row may appear twice: {seen:?}"
    );

    // Every surviving message is accounted for exactly once.
    let survivors: Vec<String> = ids.into_iter().filter(|id| *id != cursor_row).collect();
    let mut sorted_seen: Vec<String> = seen.into_iter().filter(|id| *id != cursor_row).collect();
    sorted_seen.sort();
    let mut sorted_survivors = survivors;
    sorted_survivors.sort();
    assert_eq!(sorted_seen, sorted_survivors, "no row may be skipped");
}

#[test]
fn a_page_token_is_bound_to_its_query() {
    let sim = sim();
    seed(&sim, 4);
    let listed = get(&sim, "/gmail/v1/users/me/messages?maxResults=2").json_body();
    let token = listed["nextPageToken"]
        .as_str()
        .expect("a token")
        .to_owned();

    // The same token against a *different* query is a 400, not silently page 1
    // of the new query — which is how a paging loop turns infinite.
    let wrong = get(
        &sim,
        &format!("/gmail/v1/users/me/messages?q=is:unread&maxResults=2&pageToken={token}"),
    );
    assert_eq!(wrong.status, 400);
    assert_eq!(
        wrong.json_body()["error"]["errors"][0]["reason"],
        "invalidArgument"
    );
}

#[test]
fn a_garbage_page_token_is_a_400_and_never_page_one() {
    let sim = sim();
    seed(&sim, 3);
    for token in ["", "!!!", "AAAA", "%%%%", "bm90LWEtdG9rZW4"] {
        let response = get(
            &sim,
            &format!("/gmail/v1/users/me/messages?pageToken={token}"),
        );
        assert_eq!(response.status, 400, "token {token:?}");
    }
}

#[test]
fn max_results_clamps_the_way_gmail_does() {
    let sim = sim();
    seed(&sim, 120);

    // Absent → 100.
    let default = get(&sim, "/gmail/v1/users/me/messages").json_body();
    assert_eq!(default["messages"].as_array().unwrap().len(), 100);
    // Zero → the default too.
    let zero = get(&sim, "/gmail/v1/users/me/messages?maxResults=0").json_body();
    assert_eq!(zero["messages"].as_array().unwrap().len(), 100);
    // Above the ceiling → 500 (so, here, everything).
    let huge = get(&sim, "/gmail/v1/users/me/messages?maxResults=100000").json_body();
    assert_eq!(huge["messages"].as_array().unwrap().len(), 120);
    // Junk → the default.
    let junk = get(&sim, "/gmail/v1/users/me/messages?maxResults=abc").json_body();
    assert_eq!(junk["messages"].as_array().unwrap().len(), 100);
}

// -- E6, E7, E19, E20, E21, E59, E60: history --------------------------------

#[test]
fn history_id_from_the_future_is_empty_and_not_an_error() {
    let sim = sim();
    seed(&sim, 2);
    let far = sim.history_id() + 1_000_000;
    let response = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={far}"),
    );
    // Observed, not documented: Gmail answers a future cursor with an empty
    // result carrying the mailbox's real historyId, rather than a 404. A client
    // whose stored cursor got ahead (a restored backup, a clock-skewed write)
    // should therefore find itself with nothing to do, not with a re-sync.
    assert_eq!(response.status, 200);
    let body = response.json_body();
    assert!(body.get("history").is_none(), "{body}");
    assert_eq!(body["historyId"], sim.history_id().to_string());
}

#[test]
fn history_id_older_than_the_window_is_a_404_with_gmails_body() {
    let sim = sim();
    let ancient = sim.history_id();
    sim.mailbox_mut(|mailbox| {
        mailbox
            .history_mut()
            .set_retention(HistoryRetention::Records(1));
    });
    seed(&sim, 3);

    let response = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={ancient}"),
    );
    assert_eq!(response.status, 404);
    let body = response.json_body();
    assert_eq!(body["error"]["code"], 404);
    assert_eq!(body["error"]["status"], "NOT_FOUND");
    assert_eq!(body["error"]["errors"][0]["reason"], "notFound");
    assert_eq!(body["error"]["message"], "Requested entity was not found.");
}

#[test]
fn history_paging_never_drops_or_repeats_a_record() {
    let sim = sim();
    let start = sim.history_id();
    let ids = seed(&sim, 7);
    for id in ids.iter().take(3) {
        sim.add_label(id, "STARRED").unwrap();
    }

    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut target = format!("/gmail/v1/users/me/history?startHistoryId={start}&maxResults=3");
        if let Some(value) = &token {
            target.push_str(&format!("&pageToken={value}"));
        }
        let body = get(&sim, &target).json_body();
        if let Some(records) = body["history"].as_array() {
            collected.extend(
                records
                    .iter()
                    .map(|record| record["id"].as_str().unwrap().to_owned()),
            );
        }
        match body["nextPageToken"].as_str() {
            Some(next) => token = Some(next.to_owned()),
            None => break,
        }
    }

    assert_eq!(collected.len(), 10, "7 inserts + 3 label adds");
    let mut unique = collected.clone();
    unique.dedup();
    assert_eq!(unique.len(), collected.len(), "no record twice");
    assert!(
        collected.windows(2).all(|pair| pair[0] < pair[1]),
        "records must arrive in id order across pages: {collected:?}"
    );
}

#[test]
fn history_types_filter_omits_records_with_nothing_to_say() {
    let sim = sim();
    let start = sim.history_id();
    let ids = seed(&sim, 3);
    sim.add_label(&ids[0], "STARRED").unwrap();
    sim.delete_message(&ids[1]).unwrap();

    let only_adds = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}&historyTypes=messageAdded"),
    )
    .json_body();
    let records = only_adds["history"].as_array().unwrap();
    assert_eq!(records.len(), 3, "the three inserts and nothing else");
    for record in records {
        assert!(record.get("messagesAdded").is_some());
        assert!(record.get("labelsAdded").is_none());
        assert!(record.get("messagesDeleted").is_none());
    }

    let only_deletes = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}&historyTypes=messageDeleted"),
    )
    .json_body();
    assert_eq!(only_deletes["history"].as_array().unwrap().len(), 1);

    // No `historyTypes` at all means all four.
    let everything = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}"),
    )
    .json_body();
    assert_eq!(everything["history"].as_array().unwrap().len(), 5);
}

#[test]
fn insert_then_delete_shows_both_records_in_id_order() {
    let sim = sim();
    let start = sim.history_id();
    let id = sim
        .insert_message(&MessageSpec::new().subject("brief"))
        .unwrap();
    sim.delete_message(&id).unwrap();

    let body = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}"),
    )
    .json_body();
    let records = body["history"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert!(records[0].get("messagesAdded").is_some());
    assert!(records[1].get("messagesDeleted").is_some());
    let first: u64 = records[0]["id"].as_str().unwrap().parse().unwrap();
    let second: u64 = records[1]["id"].as_str().unwrap().parse().unwrap();
    assert!(first < second);
}

#[test]
fn two_mutations_on_one_message_are_two_records() {
    let sim = sim();
    let id = sim.insert_message(&MessageSpec::new()).unwrap();
    let start = sim.history_id();
    sim.add_label(&id, "STARRED").unwrap();
    sim.mark_read(&id).unwrap();

    let body = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}"),
    )
    .json_body();
    let records = body["history"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["labelsAdded"][0]["labelIds"][0], "STARRED");
    assert_eq!(records[1]["labelsRemoved"][0]["labelIds"][0], "UNREAD");
    // The stub in each record carries the labels *after* that change.
    let after = records[1]["labelsRemoved"][0]["message"]["labelIds"]
        .as_array()
        .unwrap();
    assert!(after.iter().all(|label| label != "UNREAD"));
}

#[test]
fn a_label_removed_twice_writes_history_once() {
    let sim = sim();
    let id = sim.insert_message(&MessageSpec::new()).unwrap();
    let start = sim.history_id();

    assert!(sim.remove_label(&id, "UNREAD").unwrap().is_some());
    let after_first = sim.history_id();
    assert_eq!(sim.remove_label(&id, "UNREAD").unwrap(), None);
    assert_eq!(sim.history_id(), after_first, "the second removal is free");

    let body = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}"),
    )
    .json_body();
    assert_eq!(body["history"].as_array().unwrap().len(), 1);
}

#[test]
fn start_at_the_current_history_id_is_empty_not_a_404() {
    let sim = sim();
    seed(&sim, 2);
    let response = get(
        &sim,
        &format!(
            "/gmail/v1/users/me/history?startHistoryId={}",
            sim.history_id()
        ),
    );
    assert_eq!(response.status, 200);
    assert!(response.json_body().get("history").is_none());
}

#[test]
fn a_missing_or_unparseable_start_history_id_is_a_400() {
    let sim = sim();
    assert_eq!(get(&sim, "/gmail/v1/users/me/history").status, 400);
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/history?startHistoryId=abc").status,
        400
    );
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/history?startHistoryId=-1").status,
        400
    );
}

// -- E9, E10, E11, E34, E35, E36, E37, E38: batch ----------------------------

fn batch_body(boundary: &str, targets: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(b"Content-Type: application/http\r\n");
        out.extend_from_slice(b"Content-Transfer-Encoding: binary\r\n");
        out.extend_from_slice(format!("Content-ID: <item-{index}>\r\n\r\n").as_bytes());
        out.extend_from_slice(format!("GET {target} HTTP/1.1\r\n\r\n").as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

fn post_batch(sim: &Simulator, targets: &[String]) -> nade_gmail_sim::api::SimResponse {
    let boundary = "nade_batch_edges";
    sim.handle(
        &sim.authorized_post("/batch/gmail/v1", batch_body(boundary, targets))
            .header(
                "Content-Type",
                format!("multipart/mixed; boundary={boundary}"),
            ),
    )
}

#[test]
fn batch_of_one() {
    let sim = sim();
    let ids = seed(&sim, 1);
    let response = post_batch(
        &sim,
        &[format!("/gmail/v1/users/me/messages/{}?format=raw", ids[0])],
    );
    assert_eq!(response.status, 200);
    assert!(response
        .content_type()
        .starts_with("multipart/mixed; boundary="));
    let text = String::from_utf8(response.body).unwrap();
    assert_eq!(text.matches("Content-ID: <response-item-").count(), 1);
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 1);
}

#[test]
fn batch_of_one_hundred() {
    let sim = sim();
    let ids = seed(&sim, 100);
    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}?format=minimal"))
        .collect();
    let response = post_batch(&sim, &targets);
    assert_eq!(response.status, 200);
    let text = String::from_utf8(response.body).unwrap();
    for index in 0..100 {
        assert!(
            text.contains(&format!("Content-ID: <response-item-{index}>")),
            "part {index} is missing"
        );
    }
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 100);
    // Every sub-request paid quota, even though the envelope did not.
    assert_eq!(sim.calls().iter().filter(|call| call.in_batch).count(), 100);
}

#[test]
fn batch_over_one_hundred_is_rejected_whole() {
    let sim = sim();
    let ids = seed(&sim, 101);
    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}"))
        .collect();
    let response = post_batch(&sim, &targets);
    assert_eq!(response.status, 400);
    assert_eq!(
        response.json_body()["error"]["errors"][0]["reason"],
        "invalidArgument"
    );
}

#[test]
fn batch_where_every_subrequest_fails_is_still_a_200() {
    let sim = sim();
    let targets: Vec<String> = (0..5)
        .map(|n| format!("/gmail/v1/users/me/messages/missing{n}"))
        .collect();
    let response = post_batch(&sim, &targets);
    assert_eq!(
        response.status, 200,
        "the batch envelope succeeds even when nothing inside it did"
    );
    let text = String::from_utf8(response.body).unwrap();
    assert_eq!(text.matches("HTTP/1.1 404 Not Found").count(), 5);
    assert_eq!(text.matches("\"reason\":\"notFound\"").count(), 5);
}

#[test]
fn one_404_in_a_batch_does_not_lose_the_others() {
    let sim = sim();
    let ids = seed(&sim, 5);
    sim.delete_message(&ids[2]).unwrap();
    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}?format=raw"))
        .collect();
    let text = String::from_utf8(post_batch(&sim, &targets).body).unwrap();
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 4);
    assert_eq!(text.matches("HTTP/1.1 404 Not Found").count(), 1);
}

#[test]
fn batch_responses_come_back_out_of_order_by_default() {
    let sim = sim();
    let ids = seed(&sim, 4);
    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}?format=minimal"))
        .collect();
    let text = String::from_utf8(post_batch(&sim, &targets).body).unwrap();

    let positions: Vec<usize> = (0..4)
        .map(|n| text.find(&format!("response-item-{n}")).unwrap())
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] > pair[1]),
        "a client that zips requests to responses must break here: {positions:?}"
    );

    // Every id still landed on its own body — the correlation is sound, only
    // the order is not.
    for (index, id) in ids.iter().enumerate() {
        let marker = format!("response-item-{index}");
        let at = text.find(&marker).unwrap();
        let chunk = &text[at..];
        let end = chunk.find("\r\n--").unwrap_or(chunk.len());
        assert!(
            chunk[..end].contains(id.as_str()),
            "item-{index} carried the wrong message"
        );
    }
}

#[test]
fn batch_order_can_be_pinned_when_a_test_wants_it() {
    let sim = Simulator::with_config(Config {
        batch_order: BatchOrder::AsRequested,
        ..Config::default()
    });
    let ids = seed(&sim, 3);
    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}?format=minimal"))
        .collect();
    let text = String::from_utf8(post_batch(&sim, &targets).body).unwrap();
    let positions: Vec<usize> = (0..3)
        .map(|n| text.find(&format!("response-item-{n}")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn an_empty_or_malformed_batch_is_a_400() {
    let sim = sim();
    let empty = sim.handle(
        &sim.authorized_post("/batch/gmail/v1", b"--b--\r\n".to_vec())
            .header("Content-Type", "multipart/mixed; boundary=b"),
    );
    assert_eq!(empty.status, 400);

    let not_multipart = sim.handle(
        &sim.authorized_post("/batch/gmail/v1", b"{}".to_vec())
            .header("Content-Type", "application/json"),
    );
    assert_eq!(not_multipart.status, 400);
}

#[test]
fn a_batch_inside_a_batch_is_refused() {
    let sim = sim();
    let boundary = "outer";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Type: application/http\r\n");
    body.extend_from_slice(b"Content-ID: <nested>\r\n\r\n");
    body.extend_from_slice(b"POST /batch/gmail/v1 HTTP/1.1\r\n\r\n");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let response = sim.handle(&sim.authorized_post("/batch/gmail/v1", body).header(
        "Content-Type",
        format!("multipart/mixed; boundary={boundary}"),
    ));
    assert_eq!(response.status, 200, "the envelope still succeeds");
    let text = String::from_utf8(response.body).unwrap();
    assert!(text.contains("HTTP/1.1 400 Bad Request"));
    assert!(text.contains("Batch requests cannot be nested"));
}

// -- E12, E13, E39, E40, E41: message rendering ------------------------------

#[test]
fn unicode_and_rfc2047_survive_intact() {
    let sim = sim();
    let subject = "Föhn — Ihre Bestellung 📦 配送";
    let id = sim
        .insert_message(
            &MessageSpec::new()
                .subject(subject)
                .from("Grüße <hallo@beispiel.de>")
                .text("Don't miss out — 配送 is on its way"),
        )
        .unwrap();

    let raw = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=raw"),
    )
    .json_body();
    let bytes = nade_gmail_sim::ids::b64url_decode(raw["raw"].as_str().unwrap()).unwrap();
    let stored = sim.mailbox(|mailbox| mailbox.message(&id).unwrap().raw.clone());
    assert_eq!(
        bytes, stored,
        "raw must be byte-identical to what was stored"
    );

    // `format=full` leaves the header encoded, exactly as Gmail does — decoding
    // it is the client's job, and a simulator that pre-decoded would never
    // exercise that.
    let full = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=full"),
    )
    .json_body();
    let header = full["payload"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|header| header["name"] == "Subject")
        .unwrap();
    assert!(
        header["value"].as_str().unwrap().starts_with("=?UTF-8?B?"),
        "{header}"
    );

    // The snippet is HTML-escaped, which is real and routinely surprises people.
    assert!(
        full["snippet"].as_str().unwrap().contains("&#39;"),
        "{}",
        full["snippet"]
    );

    // …and the search index still finds the decoded words.
    let found = get(&sim, "/gmail/v1/users/me/messages?q=subject%3AF%C3%B6hn").json_body();
    assert_eq!(found["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn an_eight_megabyte_message_renders_in_every_format() {
    let sim = sim();
    let payload = vec![b'A'; 8 * 1024 * 1024];
    let id = sim
        .insert_message(
            &MessageSpec::new()
                .subject("big")
                .text("see attached")
                .attachment("big.bin", "application/octet-stream", payload.clone()),
        )
        .unwrap();

    let size = sim.mailbox(|mailbox| mailbox.message(&id).unwrap().size_estimate());
    assert!(
        size > 8 * 1024 * 1024,
        "base64 makes it bigger, not smaller"
    );

    let raw = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=raw"),
    )
    .json_body();
    let decoded = nade_gmail_sim::ids::b64url_decode(raw["raw"].as_str().unwrap()).unwrap();
    assert_eq!(decoded.len() as u64, size, "nothing was truncated");
    assert_eq!(raw["sizeEstimate"].as_u64(), Some(size));

    let full = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=full"),
    )
    .json_body();
    let attachment = &full["payload"]["parts"][1]["body"];
    assert_eq!(attachment["size"].as_u64(), Some(payload.len() as u64));
    assert!(
        attachment.get("data").is_none(),
        "attachments are fetched separately"
    );

    let attachment_id = attachment["attachmentId"].as_str().unwrap();
    let fetched = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}/attachments/{attachment_id}"),
    )
    .json_body();
    assert_eq!(fetched["size"].as_u64(), Some(payload.len() as u64));
    let bytes = nade_gmail_sim::ids::b64url_decode(fetched["data"].as_str().unwrap()).unwrap();
    assert_eq!(bytes, payload);
}

#[test]
fn metadata_headers_filters_and_metadata_carries_no_parts() {
    let sim = sim();
    let id = sim
        .insert_message(&MessageSpec::new().subject("s").text("t").html("<b>t</b>"))
        .unwrap();

    let all = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=metadata"),
    )
    .json_body();
    assert!(all["payload"].get("parts").is_none());
    assert!(all["payload"]["headers"].as_array().unwrap().len() > 2);

    let filtered = get(
        &sim,
        &format!(
            "/gmail/v1/users/me/messages/{id}?format=metadata\
             &metadataHeaders=Subject&metadataHeaders=from"
        ),
    )
    .json_body();
    let names: Vec<String> = filtered["payload"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|header| header["name"].as_str().unwrap().to_ascii_lowercase())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"subject".to_owned()));
    assert!(names.contains(&"from".to_owned()));
}

/// `format=raw` carries **no** `attachmentId`, anywhere.
///
/// This is the fact a `format=raw` sync is built on: the batch that fetches 45
/// messages as raw bytes gets no Gmail attachment ids at all, so a client has to
/// mint its own part-derived ids at parse time and resolve them with a separate
/// `format=full` fetch when the proxy actually needs the bytes. A simulator that
/// helpfully attached an id to the raw response would teach a client a Gmail
/// that does not exist, and the client's own id scheme would never be exercised.
#[test]
fn no_format_but_full_carries_an_attachment_id() {
    let sim = sim();
    let id = sim
        .insert_message(
            &MessageSpec::new()
                .text("see attached")
                .attachment("report.pdf", "application/pdf", b"%PDF-1.4".to_vec())
                .inline_attachment(
                    "logo",
                    "logo.png",
                    "image/png",
                    vec![0x89, b'P', b'N', b'G'],
                ),
        )
        .unwrap();

    for format in ["raw", "minimal", "metadata"] {
        let body = get(
            &sim,
            &format!("/gmail/v1/users/me/messages/{id}?format={format}"),
        )
        .json_body();
        let text = serde_json::to_string(&body).unwrap();
        assert!(
            !text.contains("attachmentId"),
            "format={format} must not carry an attachmentId: {text}"
        );
        if format == "raw" {
            assert!(body.get("payload").is_none(), "raw has no payload at all");
            assert!(body["raw"].is_string());
        }
    }

    // Only `format=full` has them — one per attachment part, and none on the
    // inline text part.
    let full = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=full"),
    )
    .json_body();
    let ids: Vec<&str> = full["payload"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|part| part["body"]["attachmentId"].as_str())
        .collect();
    assert_eq!(
        ids.len(),
        2,
        "the pdf and the inline png, not the text part"
    );
    assert!(full["payload"]["parts"][0]["body"]
        .get("attachmentId")
        .is_none());
}

#[test]
fn an_attachment_id_from_another_message_does_not_resolve() {
    let sim = sim();
    let first = sim
        .insert_message(&MessageSpec::new().attachment(
            "a.bin",
            "application/octet-stream",
            vec![1],
        ))
        .unwrap();
    let second = sim
        .insert_message(&MessageSpec::new().attachment(
            "b.bin",
            "application/octet-stream",
            vec![2],
        ))
        .unwrap();

    let full = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{first}?format=full"),
    )
    .json_body();
    let attachment_id = full["payload"]["parts"][1]["body"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(
        get(
            &sim,
            &format!("/gmail/v1/users/me/messages/{second}/attachments/{attachment_id}")
        )
        .status,
        404,
        "attachments are scoped to their message"
    );
    assert_eq!(
        get(
            &sim,
            &format!("/gmail/v1/users/me/messages/{first}/attachments/not-an-id")
        )
        .status,
        404
    );
}

// -- E30, E31, E32, E33: types on the wire -----------------------------------

#[test]
fn the_string_and_number_fields_are_the_way_gmail_types_them() {
    let sim = sim();
    let id = sim.insert_message(&MessageSpec::new().text("x")).unwrap();

    let profile = get(&sim, "/gmail/v1/users/me/profile").json_body();
    assert!(profile["historyId"].is_string(), "historyId is a string");
    assert!(profile["messagesTotal"].is_number());
    assert!(profile["threadsTotal"].is_number());

    let message = get(&sim, &format!("/gmail/v1/users/me/messages/{id}")).json_body();
    assert!(message["historyId"].is_string());
    assert!(
        message["internalDate"].is_string(),
        "epoch millis, as a string"
    );
    assert!(message["sizeEstimate"].is_number());
    assert!(message["payload"]["body"]["size"].is_number());

    let listed = get(&sim, "/gmail/v1/users/me/messages").json_body();
    assert!(listed["resultSizeEstimate"].is_number());

    let history = get(
        &sim,
        &format!(
            "/gmail/v1/users/me/history?startHistoryId={}",
            nade_gmail_sim::mailbox::INITIAL_HISTORY_ID
        ),
    )
    .json_body();
    assert!(history["historyId"].is_string());
    assert!(history["history"][0]["id"].is_string());

    let watch = sim
        .handle(&sim.authorized_post(
            "/gmail/v1/users/me/watch",
            br#"{"topicName":"projects/p/topics/t"}"#.to_vec(),
        ))
        .json_body();
    assert!(watch["historyId"].is_string());
    assert!(watch["expiration"].is_string(), "epoch millis, as a string");
}

// -- E44, E45: labels --------------------------------------------------------

#[test]
fn labels_list_and_get_differ_the_way_gmail_makes_them_differ() {
    let sim = sim();
    let label = sim
        .mailbox_mut(|mailbox| mailbox.create_label("Receipts"))
        .unwrap();
    let id = sim.insert_message(&MessageSpec::new()).unwrap();
    sim.add_label(&id, &label).unwrap();

    let listed = get(&sim, "/gmail/v1/users/me/labels").json_body();
    let inbox = listed["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "INBOX")
        .unwrap();
    assert_eq!(
        inbox["name"], "INBOX",
        "a system label is named after itself"
    );
    assert_eq!(inbox["type"], "system");
    assert_eq!(inbox.as_object().unwrap().len(), 3);
    assert!(
        inbox.get("messagesTotal").is_none(),
        "labels.list carries no counters"
    );

    let mine = listed["labels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == label)
        .unwrap();
    assert_eq!(mine["name"], "Receipts");
    assert_eq!(mine["type"], "user");
    assert_eq!(
        mine.as_object().unwrap().len(),
        5,
        "user labels carry visibility"
    );

    let fetched = get(&sim, &format!("/gmail/v1/users/me/labels/{label}")).json_body();
    assert_eq!(fetched["messagesTotal"], 1);
    assert_eq!(fetched["messagesUnread"], 1);
    assert_eq!(fetched["threadsTotal"], 1);

    assert_eq!(get(&sim, "/gmail/v1/users/me/labels/Label_999").status, 404);
}

#[test]
fn deleting_a_label_removes_it_everywhere_as_one_record() {
    let sim = sim();
    let label = sim
        .mailbox_mut(|mailbox| mailbox.create_label("Temp"))
        .unwrap();
    let ids = seed(&sim, 3);
    for id in &ids {
        sim.add_label(id, &label).unwrap();
    }
    let start = sim.history_id();
    sim.mailbox_mut(|mailbox| mailbox.delete_label(&label))
        .unwrap();

    let body = get(
        &sim,
        &format!("/gmail/v1/users/me/history?startHistoryId={start}"),
    )
    .json_body();
    let records = body["history"].as_array().unwrap();
    assert_eq!(records.len(), 1, "one operation, one record");
    assert_eq!(
        records[0]["labelsRemoved"].as_array().unwrap().len(),
        3,
        "one record can touch many messages, and a client must handle that"
    );
    assert_eq!(records[0]["messages"].as_array().unwrap().len(), 3);
}

#[test]
fn system_labels_cannot_be_deleted() {
    let sim = sim();
    assert!(sim
        .mailbox_mut(|mailbox| mailbox.delete_label("INBOX"))
        .is_err());
    assert_eq!(sim.mailbox(|mailbox| mailbox.labels().len()), 14);
}

// -- E22: threads ------------------------------------------------------------

#[test]
fn a_thread_grows_under_an_open_cursor() {
    let sim = sim();
    let root = sim
        .insert_message(&MessageSpec::new().subject("Question").received_days_ago(1))
        .unwrap();
    seed(&sim, 3);

    let thread_before = get(&sim, &format!("/gmail/v1/users/me/threads/{root}")).json_body();
    assert_eq!(thread_before["messages"].as_array().unwrap().len(), 1);

    let page_one = get(&sim, "/gmail/v1/users/me/threads?maxResults=2").json_body();
    let token = page_one["nextPageToken"].as_str().unwrap().to_owned();

    // A reply lands mid-listing.
    sim.insert_message(&MessageSpec::new().subject("Re: Question").reply_to(&root))
        .unwrap();

    let thread_after = get(&sim, &format!("/gmail/v1/users/me/threads/{root}")).json_body();
    assert_eq!(
        thread_after["messages"].as_array().unwrap().len(),
        2,
        "the thread grew"
    );
    assert_ne!(
        thread_after["historyId"], thread_before["historyId"],
        "and its historyId moved with it"
    );

    // Page two still resumes where page one stopped rather than restarting.
    let page_two = get(
        &sim,
        &format!("/gmail/v1/users/me/threads?maxResults=2&pageToken={token}"),
    )
    .json_body();
    let first: Vec<&str> = page_one["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    for entry in page_two["threads"].as_array().unwrap() {
        assert!(
            !first.contains(&entry["id"].as_str().unwrap()),
            "page two repeated a thread from page one"
        );
    }
    assert_eq!(get(&sim, "/gmail/v1/users/me/threads/nope").status, 404);
}

// -- E23, E24: quota ---------------------------------------------------------

#[test]
fn quota_ceiling_is_fifty_gets_per_simulated_second() {
    let sim = sim();
    let ids = seed(&sim, 1);
    sim.enable_quota();
    let target = format!("/gmail/v1/users/me/messages/{}?format=minimal", ids[0]);

    for call in 0..50 {
        assert_eq!(get(&sim, &target).status, 200, "get {call}");
    }
    let throttled = get(&sim, &target);
    assert_eq!(throttled.status, 429, "5 units each, 250 a second");
    assert_eq!(throttled.header_value("Retry-After"), Some("1"));
    assert_eq!(
        throttled.json_body()["error"]["errors"][0]["reason"],
        "rateLimitExceeded"
    );

    // Nothing refills until the clock moves.
    assert_eq!(get(&sim, &target).status, 429);
    sim.clock().advance(Duration::from_millis(20));
    assert_eq!(get(&sim, &target).status, 200, "20 ms buys exactly one get");
    assert_eq!(get(&sim, &target).status, 429);
}

#[test]
fn the_over_quota_error_can_be_a_403_instead() {
    let sim = sim();
    seed(&sim, 1);
    sim.quota_mut(|quota| {
        quota.enabled = true;
        quota.over_quota = OverQuota::UserRateLimit;
    });
    for _ in 0..50 {
        get(&sim, "/gmail/v1/users/me/messages");
    }
    let refused = get(&sim, "/gmail/v1/users/me/messages");
    assert_eq!(refused.status, 403);
    assert_eq!(
        refused.json_body()["error"]["errors"][0]["reason"],
        "userRateLimitExceeded"
    );
}

#[test]
fn each_endpoint_debits_its_own_published_cost() {
    assert_eq!(cost::MESSAGES_GET, 5);
    assert_eq!(cost::MESSAGES_LIST, 5);
    assert_eq!(cost::HISTORY_LIST, 2);
    assert_eq!(cost::GET_PROFILE, 1);

    let sim = sim();
    sim.enable_quota();
    // 250 profile calls at 1 unit each fit in one simulated second; 50 gets do
    // not leave room for a 51st.
    for _ in 0..250 {
        assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);
    }
    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 429);
}

// -- E25, E26, E27, E57, E58: faults -----------------------------------------

#[test]
fn injected_429_carries_retry_after_and_fires_on_exactly_the_nth_call() {
    let sim = sim();
    sim.inject(
        FaultRule::new(Fault::Throttle {
            retry_after_seconds: Some(7),
        })
        .on_path_containing("/messages")
        .on_nth_call(2),
    );

    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);
    assert_eq!(get(&sim, "/gmail/v1/users/me/messages").status, 200);
    let throttled = get(&sim, "/gmail/v1/users/me/messages");
    assert_eq!(throttled.status, 429);
    assert_eq!(throttled.header_value("Retry-After"), Some("7"));
    assert_eq!(get(&sim, "/gmail/v1/users/me/messages").status, 200);
}

#[test]
fn the_two_403_rate_limits_and_the_429_are_all_distinguishable() {
    let sim = sim();
    for (fault, status, reason) in [
        (
            Fault::Fail(ApiError::RateLimitExceeded),
            403,
            "rateLimitExceeded",
        ),
        (
            Fault::Fail(ApiError::UserRateLimitExceeded),
            403,
            "userRateLimitExceeded",
        ),
        (
            Fault::Throttle {
                retry_after_seconds: None,
            },
            429,
            "rateLimitExceeded",
        ),
    ] {
        sim.clear_faults();
        sim.inject(FaultRule::new(fault).times(1));
        let response = get(&sim, "/gmail/v1/users/me/profile");
        assert_eq!(response.status, status);
        assert_eq!(response.json_body()["error"]["errors"][0]["reason"], reason);
    }
}

#[test]
fn five_hundred_and_five_oh_three_look_like_gmails() {
    let sim = sim();
    sim.inject(FaultRule::new(Fault::Fail(ApiError::BackendError)).times(1));
    let five_hundred = get(&sim, "/gmail/v1/users/me/profile");
    assert_eq!(five_hundred.status, 500);
    assert_eq!(five_hundred.json_body()["error"]["status"], "INTERNAL");

    sim.clear_faults();
    sim.inject(
        FaultRule::new(Fault::Unavailable {
            retry_after_seconds: Some(30),
        })
        .times(1),
    );
    let unavailable = get(&sim, "/gmail/v1/users/me/profile");
    assert_eq!(unavailable.status, 503);
    assert_eq!(unavailable.header_value("Retry-After"), Some("30"));
    assert_eq!(unavailable.json_body()["error"]["status"], "UNAVAILABLE");
}

#[test]
fn a_slow_fault_reports_latency_without_the_in_process_path_sleeping() {
    let sim = sim();
    sim.inject(FaultRule::new(Fault::Delay(Duration::from_secs(30))).times(1));
    let started = std::time::Instant::now();
    let response = get(&sim, "/gmail/v1/users/me/profile");
    assert_eq!(response.status, 200, "a delay is not a failure");
    assert_eq!(response.latency, Duration::from_secs(30));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the in-process transport must not really wait"
    );
}

#[test]
fn a_fault_can_target_only_the_calls_inside_a_batch() {
    let sim = sim();
    let ids = seed(&sim, 3);
    sim.inject(FaultRule::new(Fault::Fail(ApiError::ServiceUnavailable)).in_batch(true));
    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);

    let targets: Vec<String> = ids
        .iter()
        .map(|id| format!("/gmail/v1/users/me/messages/{id}"))
        .collect();
    let text = String::from_utf8(post_batch(&sim, &targets).body).unwrap();
    assert_eq!(text.matches("HTTP/1.1 503").count(), 3);
}

// -- E28, E29: auth ----------------------------------------------------------

#[test]
fn expired_token_then_refresh_then_success() {
    let sim = sim();
    seed(&sim, 1);
    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);

    sim.expire_access_token();
    let refused = get(&sim, "/gmail/v1/users/me/profile");
    assert_eq!(refused.status, 401);
    let body = refused.json_body();
    assert_eq!(body["error"]["errors"][0]["reason"], "authError");
    assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
    assert_eq!(body["error"]["errors"][0]["location"], "Authorization");

    let old_refresh = sim.refresh_token();
    let refreshed = sim.handle(&SimRequest::post(
        "/token",
        format!("grant_type=refresh_token&refresh_token={old_refresh}&client_id=c&client_secret=s"),
    ));
    assert_eq!(refreshed.status, 200);
    let tokens = refreshed.json_body();
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3599);
    assert!(
        tokens["refresh_token"].is_string(),
        "rotation: the client must persist the new one"
    );
    assert_ne!(tokens["refresh_token"], old_refresh);

    // The new access token works; the old refresh token no longer does.
    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);
    let stale = sim.handle(&SimRequest::post(
        "/token",
        format!("grant_type=refresh_token&refresh_token={old_refresh}"),
    ));
    assert_eq!(stale.status, 400);
    assert_eq!(stale.json_body()["error"], "invalid_grant");
}

#[test]
fn invalid_grant_on_refresh_is_the_needs_reauth_path() {
    let sim = sim();
    sim.revoke_refresh_token();
    let refused = sim.handle(&SimRequest::post(
        "/token",
        format!(
            "grant_type=refresh_token&refresh_token={}",
            sim.refresh_token()
        ),
    ));
    assert_eq!(refused.status, 400);
    let body = refused.json_body();
    assert_eq!(body["error"], "invalid_grant");
    assert_eq!(
        body["error_description"],
        "Token has been expired or revoked."
    );

    // The wrong grant type is a different, distinguishable error.
    let wrong = sim.handle(&SimRequest::post("/token", "grant_type=authorization_code"));
    assert_eq!(wrong.json_body()["error"], "unsupported_grant_type");
}

#[test]
fn a_wrong_or_missing_bearer_token_is_the_same_401() {
    let sim = sim();
    for request in [
        SimRequest::get("/gmail/v1/users/me/profile"),
        SimRequest::get("/gmail/v1/users/me/profile").bearer("ya29.wrong"),
        SimRequest::get("/gmail/v1/users/me/profile").header("Authorization", "Basic abc"),
    ] {
        let response = sim.handle(&request);
        assert_eq!(response.status, 401);
        assert_eq!(
            response.json_body()["error"]["errors"][0]["reason"],
            "authError"
        );
    }
}

#[test]
fn auth_can_be_turned_off_for_tests_that_are_about_something_else() {
    let sim = Simulator::with_config(Config {
        auth: AuthMode::Off,
        ..Config::default()
    });
    assert_eq!(
        sim.handle(&SimRequest::get("/gmail/v1/users/me/profile"))
            .status,
        200
    );
}

// -- E53, E54: watch ---------------------------------------------------------

#[test]
fn watch_returns_a_seven_day_expiry_and_stop_is_a_204() {
    let sim = sim();
    let now = sim.clock().now_ms();
    let response = sim.handle(&sim.authorized_post(
        "/gmail/v1/users/me/watch",
        br#"{"topicName":"projects/p/topics/gmail-events","labelIds":["INBOX"]}"#.to_vec(),
    ));
    assert_eq!(response.status, 200);
    let body = response.json_body();
    let expiration: i64 = body["expiration"].as_str().unwrap().parse().unwrap();
    assert_eq!(expiration, now + 7 * DAY_MS);
    assert_eq!(body["historyId"], sim.history_id().to_string());

    let watch = sim.watch().expect("a registration");
    assert_eq!(watch.topic_name, "projects/p/topics/gmail-events");
    assert_eq!(watch.label_ids, ["INBOX"]);

    // Stopping without a watch is fine, and stopping twice is fine.
    assert_eq!(
        sim.handle(&sim.authorized_post("/gmail/v1/users/me/stop", Vec::new()))
            .status,
        204
    );
    assert!(sim.watch().is_none());
    assert_eq!(
        sim.handle(&sim.authorized_post("/gmail/v1/users/me/stop", Vec::new()))
            .status,
        204
    );

    // A watch with no topic is a 400.
    assert_eq!(
        sim.handle(&sim.authorized_post("/gmail/v1/users/me/watch", b"{}".to_vec()))
            .status,
        400
    );
}

// -- E47, E55: determinism and concurrency -----------------------------------

#[test]
fn the_clock_only_moves_when_told() {
    let sim = sim();
    let start = sim.clock().now_ms();
    seed(&sim, 2);
    let history_after_seed = sim.history_id();
    let watch_first = sim
        .handle(&sim.authorized_post(
            "/gmail/v1/users/me/watch",
            br#"{"topicName":"projects/p/topics/t"}"#.to_vec(),
        ))
        .json_body();

    for _ in 0..200 {
        let _ = get(&sim, "/gmail/v1/users/me/messages");
        let _ = get(&sim, "/gmail/v1/users/me/profile");
    }

    assert_eq!(sim.clock().now_ms(), start, "reads must not move time");
    assert_eq!(
        sim.history_id(),
        history_after_seed,
        "reads must not write history"
    );
    let watch_again = sim
        .handle(&sim.authorized_post(
            "/gmail/v1/users/me/watch",
            br#"{"topicName":"projects/p/topics/t"}"#.to_vec(),
        ))
        .json_body();
    assert_eq!(watch_first["expiration"], watch_again["expiration"]);
}

#[test]
fn the_simulator_is_send_and_sync_and_reads_never_see_half_a_mutation() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Simulator>();

    let sim = Arc::new(sim());
    let ids = seed(&sim, 20);
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let sim = Arc::clone(&sim);
            std::thread::spawn(move || {
                for _ in 0..200 {
                    let body = sim
                        .handle(&sim.authorized("/gmail/v1/users/me/messages?maxResults=500"))
                        .json_body();
                    let rows = body["messages"].as_array().cloned().unwrap_or_default();
                    // Whatever the writer is doing, one response is internally
                    // consistent with itself: the key is present exactly when
                    // there are rows, and no message appears twice.
                    assert_eq!(rows.is_empty(), body.get("messages").is_none());
                    let mut ids: Vec<&str> =
                        rows.iter().filter_map(|row| row["id"].as_str()).collect();
                    assert_eq!(ids.len(), rows.len(), "every row must carry an id");
                    ids.sort_unstable();
                    let before = ids.len();
                    ids.dedup();
                    assert_eq!(ids.len(), before, "a row appeared twice in one page");
                }
            })
        })
        .collect();

    let writer = {
        let sim = Arc::clone(&sim);
        std::thread::spawn(move || {
            for id in ids {
                let _ = sim.add_label(&id, "STARRED");
                let _ = sim.mark_read(&id);
            }
        })
    };

    writer.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
}

// -- Routing -----------------------------------------------------------------

#[test]
fn another_users_mailbox_is_a_404_and_the_address_form_works() {
    let sim = sim();
    seed(&sim, 1);
    assert_eq!(get(&sim, "/gmail/v1/users/me/profile").status, 200);
    assert_eq!(
        get(&sim, "/gmail/v1/users/me%40example.com/profile").status,
        200,
        "the authenticated address is a valid userId"
    );
    assert_eq!(
        get(&sim, "/gmail/v1/users/someone%40else.com/profile").status,
        404
    );
    assert_eq!(get(&sim, "/gmail/v1/users/me/nonsense").status, 404);
    assert_eq!(get(&sim, "/not/an/api/path").status, 404);
    assert_eq!(
        sim.handle(&sim.authorized_post("/gmail/v1/users/me/profile", Vec::new()))
            .status,
        404,
        "the wrong method is not a route"
    );
    assert_eq!(
        sim.handle(
            &SimRequest::new(Method::Delete, "/gmail/v1/users/me/messages/x", Vec::new())
                .bearer(&sim.access_token())
        )
        .status,
        404,
        "v1 takes no outbound actions, so there is no delete endpoint"
    );
}

#[test]
fn the_call_log_records_what_the_client_actually_did() {
    let sim = sim();
    let ids = seed(&sim, 2);
    sim.clear_calls();

    get(&sim, "/gmail/v1/users/me/profile");
    get(&sim, "/gmail/v1/users/me/messages?q=newer_than%3A30d");
    post_batch(
        &sim,
        &[format!("/gmail/v1/users/me/messages/{}?format=raw", ids[0])],
    );

    let calls = sim.calls();
    assert_eq!(calls.len(), 4, "profile, list, the sub-request, the batch");
    assert_eq!(calls[0].target, "/gmail/v1/users/me/profile");
    assert!(calls[1].target.contains("q=newer_than%3A30d"));
    assert!(
        calls[2].in_batch,
        "sub-requests are logged before the envelope"
    );
    assert_eq!(calls[3].target, "/batch/gmail/v1");
    assert!(calls.iter().all(|call| call.status == 200));
}

// -- E42, E43, E52: the conformance corpus through the API -------------------

#[test]
fn the_whole_mime_corpus_serves_over_the_api_without_losing_a_byte() {
    let sim = sim();
    let seeded = sim.mailbox_mut(|mailbox| {
        nade_gmail_sim::seed::seed_mime_corpus(
            mailbox,
            &nade_gmail_sim::seed::SeedOptions::default(),
        )
        .expect("the corpus is committed")
    });
    assert_eq!(seeded.len(), 26);

    for entry in &seeded {
        // EDGE: case 25 has mixed CRLF/LF line endings and case 13 has no Date
        // header. `format=raw` must return the file's bytes exactly either way —
        // no normalisation, no re-wrapping.
        let stored = sim.mailbox(|mailbox| mailbox.message(&entry.id).unwrap().raw.clone());
        let raw = get(
            &sim,
            &format!("/gmail/v1/users/me/messages/{}?format=raw", entry.id),
        )
        .json_body();
        let decoded = nade_gmail_sim::ids::b64url_decode(raw["raw"].as_str().unwrap()).unwrap();
        assert_eq!(decoded, stored, "{} did not round-trip", entry.file);

        // …and every other format renders without panicking.
        for format in ["minimal", "metadata", "full"] {
            let body = get(
                &sim,
                &format!("/gmail/v1/users/me/messages/{}?format={format}", entry.id),
            );
            assert_eq!(body.status, 200, "{} as {format}", entry.file);
            assert!(body.json_body()["internalDate"].is_string());
        }
    }

    let listed = get(&sim, "/gmail/v1/users/me/messages?maxResults=500").json_body();
    assert_eq!(listed["messages"].as_array().unwrap().len(), 26);
}

#[test]
fn a_message_with_mixed_line_endings_keeps_them() {
    let sim = sim();
    let raw = b"Subject: mixed\nFrom: a@b.c\r\nTo: d@e.f\n\r\nline one\r\nline two\n".to_vec();
    let id = sim
        .insert_message(&MessageSpec::from_raw(raw.clone()))
        .unwrap();
    let body = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=raw"),
    )
    .json_body();
    let decoded = nade_gmail_sim::ids::b64url_decode(body["raw"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, raw, "raw is verbatim, never normalised");
    // The part tree still found the boundary, despite the ragged endings.
    let full = get(
        &sim,
        &format!("/gmail/v1/users/me/messages/{id}?format=full"),
    )
    .json_body();
    assert_eq!(full["payload"]["headers"].as_array().unwrap().len(), 3);
}

// -- E35, E36: awkward batches ----------------------------------------------

#[test]
fn an_unroutable_batch_subrequest_is_a_404_part_beside_the_successes() {
    let sim = sim();
    let ids = seed(&sim, 2);
    let targets = vec![
        format!("/gmail/v1/users/me/messages/{}", ids[0]),
        "/not/an/api/path".to_owned(),
        format!("/gmail/v1/users/me/messages/{}", ids[1]),
    ];
    let text = String::from_utf8(post_batch(&sim, &targets).body).unwrap();
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 2);
    assert_eq!(text.matches("HTTP/1.1 404 Not Found").count(), 1);
}

#[test]
fn duplicate_content_ids_in_a_batch_are_passed_through_not_hidden() {
    let sim = sim();
    let ids = seed(&sim, 2);
    let boundary = "dupe";
    let mut body = Vec::new();
    for id in &ids {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Type: application/http\r\n");
        body.extend_from_slice(b"Content-ID: <same>\r\n\r\n");
        body.extend_from_slice(
            format!("GET /gmail/v1/users/me/messages/{id} HTTP/1.1\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let response = sim.handle(&sim.authorized_post("/batch/gmail/v1", body).header(
        "Content-Type",
        format!("multipart/mixed; boundary={boundary}"),
    ));
    let text = String::from_utf8(response.body).unwrap();
    // Two parts, both labelled `<response-same>`. The simulator does not
    // deduplicate or rename them: a client that indexes by Content-ID silently
    // loses one answer, and that is the bug worth surfacing.
    assert_eq!(text.matches("Content-ID: <response-same>").count(), 2);
    assert_eq!(text.matches("HTTP/1.1 200 OK").count(), 2);
}

// -- q support at the API surface -------------------------------------------

#[test]
fn the_query_operators_the_brief_requires_all_work_over_the_wire() {
    let sim = sim();
    let now = sim.clock().now_ms();
    let recent = sim
        .insert_message(
            &MessageSpec::new()
                .subject("Recent invoice")
                .from("billing@stripe.com")
                .text("due now")
                .received_at(ReceivedAt::AtMs(now - DAY_MS)),
        )
        .unwrap();
    let old = sim
        .insert_message(
            &MessageSpec::new()
                .subject("Ancient")
                .from("someone@else.com")
                .text("old")
                .received_at(ReceivedAt::AtMs(now - 60 * DAY_MS)),
        )
        .unwrap();
    sim.mark_read(&old).unwrap();
    let label = sim
        .mailbox_mut(|mailbox| mailbox.create_label("Receipts"))
        .unwrap();
    sim.add_label(&recent, &label).unwrap();

    let ids = |q: &str| -> Vec<String> {
        let target = format!(
            "/gmail/v1/users/me/messages?q={}",
            nade_gmail_sim::api::encode_component(q)
        );
        get(&sim, &target).json_body()["messages"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| row["id"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };

    assert_eq!(ids("newer_than:30d"), std::slice::from_ref(&recent));
    assert_eq!(ids("older_than:30d"), std::slice::from_ref(&old));
    assert_eq!(ids("from:stripe"), std::slice::from_ref(&recent));
    assert_eq!(ids("is:unread"), std::slice::from_ref(&recent));
    assert_eq!(ids("is:read"), std::slice::from_ref(&old));
    assert_eq!(ids("label:receipts"), std::slice::from_ref(&recent));
    assert_eq!(ids("label:inbox").len(), 2);
    assert_eq!(
        ids("newer_than:30d from:stripe is:unread"),
        std::slice::from_ref(&recent)
    );
    // Both senders, newest first — the listing order, not the query's order.
    assert_eq!(
        ids("from:stripe OR from:else"),
        [recent.clone(), old.clone()].as_slice()
    );
    assert_eq!(
        ids("{from:stripe from:else}").len(),
        2,
        "brace grouping is OR"
    );
    assert_eq!(ids("-from:stripe"), std::slice::from_ref(&old));
    assert!(
        ids("wibble:wobble").is_empty(),
        "unknown operators never 400"
    );
    assert_eq!(ids("").len(), 2, "an empty q matches everything");
    // The whole thing is still a 200, whatever the query said.
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/messages?q=%22unbalanced").status,
        200
    );
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/messages?q=%28%28%28").status,
        200
    );
}

#[test]
fn a_thread_listing_previews_its_newest_message() {
    let sim = sim();
    let root = sim
        .insert_message(
            &MessageSpec::new()
                .subject("Question")
                .text("the original question")
                .received_days_ago(2),
        )
        .unwrap();
    sim.insert_message(
        &MessageSpec::new()
            .subject("Re: Question")
            .text("the latest reply")
            .reply_to(&root)
            .received_days_ago(1),
    )
    .unwrap();

    let listed = get(&sim, "/gmail/v1/users/me/threads").json_body();
    let entry = &listed["threads"][0];
    assert_eq!(entry["id"], root);
    assert!(
        entry["snippet"]
            .as_str()
            .unwrap()
            .contains("the latest reply"),
        "a thread previews its newest message, not its oldest: {entry}"
    );
    // The thread's historyId tracks the newest change to any of its messages.
    let thread = get(&sim, &format!("/gmail/v1/users/me/threads/{root}")).json_body();
    assert_eq!(entry["historyId"], thread["historyId"]);
    assert_eq!(thread["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        thread["messages"][0]["id"], root,
        "threads.get reads oldest first"
    );
}

#[test]
fn an_unknown_label_id_is_a_400_not_an_empty_result() {
    let sim = sim();
    seed(&sim, 3);

    // `labelIds` is case-SENSITIVE, unlike `q=label:`. Gmail answers
    // `Invalid label: sent` and means `SENT`. Returning an empty page instead
    // would make a client that lowercases its label ids look at an empty
    // mailbox and believe it.
    let refused = get(&sim, "/gmail/v1/users/me/messages?labelIds=inbox");
    assert_eq!(refused.status, 400);
    let body = refused.json_body();
    assert_eq!(body["error"]["errors"][0]["reason"], "invalidArgument");
    assert_eq!(body["error"]["message"], "Invalid label: inbox");

    assert_eq!(
        get(&sim, "/gmail/v1/users/me/messages?labelIds=INBOX").status,
        200
    );
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/messages?labelIds=Label_404").status,
        400
    );
    // …and it applies to threads.list too.
    assert_eq!(
        get(&sim, "/gmail/v1/users/me/threads?labelIds=inbox").status,
        400
    );
}

#[test]
fn a_malformed_query_is_indistinguishable_from_an_empty_inbox() {
    let sim = sim();
    seed(&sim, 3);
    // Every one of these is a 200 with no `messages` key — never a 400. That is
    // real, and it is why a client must validate `q` before sending it.
    for malformed in [
        "%22from%3Ax%20is%3Aunread%22", // the whole query quoted
        "wibble%3Awobble",
        "newer_than%3A1w",        // an undocumented unit
        "from%3A%20stripe",       // a space after the colon
        "after%3A08%2F01%2F2026", // the ambiguous documented date order
    ] {
        let response = get(&sim, &format!("/gmail/v1/users/me/messages?q={malformed}"));
        assert_eq!(response.status, 200, "q={malformed} must not 400");
        let body = response.json_body();
        assert!(
            body.get("messages").is_none(),
            "q={malformed} matched something: {body}"
        );
        assert_eq!(body["resultSizeEstimate"], 0);
    }
}

/// `resultSizeEstimate` saturates and cannot be used to compare two queries.
///
/// MEASURED against a real mailbox: scoped to `newer_than:3d` (85 messages), the
/// field reported **201 for every query tried**, including the empty one and
/// including queries whose actual result counts were 1, 6, 35 and 59. It is not
/// a total, not a page size, and not comparable between queries.
#[test]
fn result_size_estimate_saturates_and_cannot_compare_two_queries() {
    let sim = sim();
    let now = sim.clock().now_ms();
    for n in 0..40 {
        sim.insert_message(
            &MessageSpec::new()
                .subject(format!("m{n}"))
                .from(if n < 3 {
                    "a@one.example"
                } else {
                    "b@two.example"
                })
                .received_at(ReceivedAt::AtMs(now - i64::from(n) * 3_600_000)),
        )
        .unwrap();
    }

    let estimate = |q: &str| -> u64 {
        let target = format!(
            "/gmail/v1/users/me/messages?q={}",
            nade_gmail_sim::api::encode_component(q)
        );
        get(&sim, &target).json_body()["resultSizeEstimate"]
            .as_u64()
            .unwrap()
    };
    let count = |q: &str| -> usize {
        let target = format!(
            "/gmail/v1/users/me/messages?q={}",
            nade_gmail_sim::api::encode_component(q)
        );
        get(&sim, &target).json_body()["messages"]
            .as_array()
            .map_or(0, Vec::len)
    };

    // Three queries with genuinely different result counts…
    assert_eq!(count(""), 40);
    assert_eq!(count("from:one.example"), 3);
    assert_eq!(count("from:two.example"), 37);

    // …all report the same estimate. Comparing them tells you nothing.
    assert_eq!(estimate(""), estimate("from:one.example"));
    assert_eq!(estimate(""), estimate("from:two.example"));
    assert_ne!(
        estimate("from:one.example") as usize,
        count("from:one.example")
    );

    // Only a query that matches nothing reports 0 — the one case where the
    // field carries information.
    assert_eq!(estimate("from:nobody.example"), 0);
    assert_eq!(count("from:nobody.example"), 0);

    // A test that needs the `list.json` fixture's shape can ask for it.
    let paged = Simulator::with_config(Config {
        result_size_estimate: nade_gmail_sim::render::ResultSizeEstimate::PageBased,
        ..Config::default()
    });
    seed(&paged, 5);
    let body = paged
        .handle(&paged.authorized("/gmail/v1/users/me/messages?maxResults=4"))
        .json_body();
    assert_eq!(
        body["resultSizeEstimate"], 5,
        "returned + 1 when more exist"
    );
}

/// A single page caps at 500 ids, so any count comparison must stay under it or
/// it silently compares two ceilings.
#[test]
fn a_single_page_caps_at_five_hundred_ids() {
    let sim = sim();
    let now = sim.clock().now_ms();
    for n in 0..520 {
        sim.insert_message(
            &MessageSpec::new()
                .subject(format!("m{n}"))
                .received_at(ReceivedAt::AtMs(now - i64::from(n) * 60_000)),
        )
        .unwrap();
    }

    let body = get(&sim, "/gmail/v1/users/me/messages?maxResults=1000").json_body();
    assert_eq!(
        body["messages"].as_array().unwrap().len(),
        500,
        "asking for more than 500 still yields 500"
    );
    assert!(
        body["nextPageToken"].is_string(),
        "the other 20 are behind a token, not lost"
    );

    // Two different mailbox sizes both hit the ceiling, so counting one page is
    // not a way to compare them.
    let smaller = Simulator::new();
    let now = smaller.clock().now_ms();
    for n in 0..505 {
        smaller
            .insert_message(
                &MessageSpec::new().received_at(ReceivedAt::AtMs(now - i64::from(n) * 60_000)),
            )
            .unwrap();
    }
    let other = get(&smaller, "/gmail/v1/users/me/messages?maxResults=500").json_body();
    assert_eq!(other["messages"].as_array().unwrap().len(), 500);
}
