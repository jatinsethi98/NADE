//! The scenario this crate exists for, written as a test.
//!
//! **Initial sync → three mutations → incremental history → replay the same
//! history page → assert the second application changed nothing.**
//!
//! A static stub cannot express any of this, because the second `history.list`
//! would return the same canned bytes without knowing it was the second, and the
//! "nothing changed" assertion would be about the stub rather than about the
//! client.
//!
//! `MiniStore` below is a deliberately small stand-in for NADE's `messages`
//! table: enough state to make idempotency a real question, and no more.

use std::{collections::BTreeMap, time::Duration};

use nade_gmail_sim::{
    history::HistoryRetention, query::Query, MessageSpec, Scenario, Simulator, Target,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// A minimal client: exactly the state a sync has to keep
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Row {
    thread_id: String,
    labels: Vec<String>,
    size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MiniStore {
    rows: BTreeMap<String, Row>,
    last_history_id: Option<u64>,
    /// How many times `upsert` ran. Not part of equality: a correct client may
    /// re-apply an idempotent change, and the point is that the *state* is the
    /// same afterwards.
    upserts: usize,
    deletes: usize,
}

impl MiniStore {
    fn upsert(&mut self, id: &str, row: Row) {
        self.upserts += 1;
        self.rows.insert(id.to_owned(), row);
    }

    fn delete(&mut self, id: &str) {
        if self.rows.remove(id).is_some() {
            self.deletes += 1;
        }
    }

    /// State only. Counters are excluded on purpose — see `upserts`.
    fn state(&self) -> (&BTreeMap<String, Row>, Option<u64>) {
        (&self.rows, self.last_history_id)
    }
}

fn row_from_message(body: &Value) -> Row {
    Row {
        thread_id: body["threadId"].as_str().unwrap_or_default().to_owned(),
        labels: body["labelIds"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        size: body["sizeEstimate"].as_u64().unwrap_or_default(),
    }
}

fn labels_of(stub: &Value) -> Vec<String> {
    stub["labelIds"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// An initial sync: `getProfile` for the cursor **first** (overlap, not gap),
/// then `messages.list`, then `messages.get` for each id.
fn initial_sync(sim: &Simulator, store: &mut MiniStore, q: &str) {
    let profile = sim
        .handle(&sim.authorized("/gmail/v1/users/me/profile"))
        .json_body();
    let cursor: u64 = profile["historyId"]
        .as_str()
        .expect("historyId is a string")
        .parse()
        .expect("…holding a number");

    let mut page_token: Option<String> = None;
    let mut ids: Vec<String> = Vec::new();
    loop {
        let mut target = format!(
            "/gmail/v1/users/me/messages?q={}&maxResults=2",
            nade_gmail_sim::api::encode_component(q)
        );
        if let Some(token) = &page_token {
            target.push_str(&format!("&pageToken={token}"));
        }
        let body = sim.handle(&sim.authorized(&target)).json_body();
        if let Some(rows) = body["messages"].as_array() {
            ids.extend(
                rows.iter()
                    .filter_map(|row| row["id"].as_str())
                    .map(ToOwned::to_owned),
            );
        }
        match body["nextPageToken"].as_str() {
            Some(token) => page_token = Some(token.to_owned()),
            None => break,
        }
    }

    for id in ids {
        let response =
            sim.handle(&sim.authorized(&format!("/gmail/v1/users/me/messages/{id}?format=raw")));
        // A message deleted between the list and the get is normal, not an error.
        if response.status == 404 {
            continue;
        }
        store.upsert(&id, row_from_message(&response.json_body()));
    }
    store.last_history_id = Some(cursor);
}

/// Fetch one history page and hand back its raw JSON, without applying it.
fn fetch_history_page(sim: &Simulator, start: u64) -> nade_gmail_sim::api::SimResponse {
    sim.handle(&sim.authorized(&format!(
        "/gmail/v1/users/me/history?startHistoryId={start}\
         &historyTypes=messageAdded&historyTypes=messageDeleted\
         &historyTypes=labelAdded&historyTypes=labelRemoved"
    )))
}

/// Apply one history page. Written to be idempotent: every effect is an upsert
/// keyed by message id, or a delete, and the cursor only ever moves forward.
fn apply_history_page(sim: &Simulator, store: &mut MiniStore, page: &Value) {
    let Some(records) = page["history"].as_array() else {
        return;
    };
    for record in records {
        let record_id: u64 = record["id"]
            .as_str()
            .expect("record id is a string")
            .parse()
            .expect("…holding a number");

        for added in record["messagesAdded"].as_array().unwrap_or(&Vec::new()) {
            let id = added["message"]["id"].as_str().unwrap_or_default();
            let response = sim
                .handle(&sim.authorized(&format!("/gmail/v1/users/me/messages/{id}?format=raw")));
            if response.is_success() {
                store.upsert(id, row_from_message(&response.json_body()));
            }
        }
        for removed in record["messagesDeleted"].as_array().unwrap_or(&Vec::new()) {
            store.delete(removed["message"]["id"].as_str().unwrap_or_default());
        }
        for change in record["labelsAdded"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .chain(record["labelsRemoved"].as_array().unwrap_or(&Vec::new()))
        {
            let stub = &change["message"];
            let id = stub["id"].as_str().unwrap_or_default();
            // The record already carries the labels *after* the change, so the
            // right move is to overwrite rather than to add or subtract. That is
            // what makes replay free.
            if let Some(row) = store.rows.get_mut(id) {
                row.labels = labels_of(stub);
                store.upserts += 1;
            }
        }

        // Advance per record, never to the page's top-level `historyId`.
        store.last_history_id = Some(
            store
                .last_history_id
                .map_or(record_id, |current| current.max(record_id)),
        );
    }
}

/// The mailbox as the store should see it, for comparison.
fn expected_rows(sim: &Simulator) -> BTreeMap<String, Row> {
    sim.mailbox(|mailbox| {
        mailbox
            .messages_newest_first()
            .into_iter()
            .filter(|message| !message.has_label("TRASH") && !message.has_label("SPAM"))
            .map(|message| {
                (
                    message.id.clone(),
                    Row {
                        thread_id: message.thread_id.clone(),
                        labels: message.label_ids.clone(),
                        size: message.size_estimate(),
                    },
                )
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------
// The headline test
// ---------------------------------------------------------------------------

#[test]
fn initial_sync_then_incremental_then_replay_changes_nothing() {
    let sim = Simulator::new();

    // --- the world before the sync ---------------------------------------
    let seeded = Scenario::new()
        .note("three messages already in the mailbox")
        .insert(
            MessageSpec::new()
                .subject("Invoice 41")
                .from("billing@stripe.com")
                .text("first")
                .received_days_ago(3),
        )
        .insert(
            MessageSpec::new()
                .subject("Standup notes")
                .from("team@example.com")
                .text("second")
                .received_days_ago(2),
        )
        .insert(
            MessageSpec::new()
                .subject("Delivery update")
                .from("noreply@shop.example")
                .html_only("<p>third</p>")
                .received_days_ago(1),
        )
        .run(&sim)
        .expect("seed applies");
    assert_eq!(seeded.inserted.len(), 3);

    // --- 1. initial sync --------------------------------------------------
    let mut store = MiniStore::default();
    initial_sync(&sim, &mut store, "newer_than:30d");

    assert_eq!(store.rows.len(), 3, "initial sync must produce three rows");
    assert_eq!(store.rows, expected_rows(&sim));
    let cursor_after_initial = store.last_history_id.expect("a cursor");
    assert_eq!(cursor_after_initial, sim.history_id());

    // --- 2. three mutations ----------------------------------------------
    let after_mutations = Scenario::new()
        .note("a new message arrives")
        .insert(
            MessageSpec::new()
                .subject("Invoice 42")
                .from("billing@stripe.com")
                .text("fourth"),
        )
        .note("an old one is read")
        .mark_read(Target::Id(seeded.inserted[0].clone()))
        .note("and one is deleted outright")
        .delete(Target::Id(seeded.inserted[1].clone()))
        .run(&sim)
        .expect("mutations apply");
    assert_eq!(
        after_mutations.history_ids.len(),
        3,
        "three mutations, three history records"
    );

    // --- 3. incremental history ------------------------------------------
    let page = fetch_history_page(&sim, cursor_after_initial);
    assert_eq!(page.status, 200);
    let page_json = page.json_body();
    assert_eq!(
        page_json["history"].as_array().map(Vec::len),
        Some(3),
        "the page must carry all three records: {page_json}"
    );

    apply_history_page(&sim, &mut store, &page_json);

    assert_eq!(store.rows.len(), 3, "one added, one deleted");
    assert_eq!(
        store.rows,
        expected_rows(&sim),
        "the store must match the mailbox after the incremental sync"
    );
    assert!(!store.rows.contains_key(&seeded.inserted[1]));
    assert!(
        !store.rows[&seeded.inserted[0]]
            .labels
            .iter()
            .any(|label| label == "UNREAD"),
        "the read message must have lost UNREAD"
    );

    let state_after_first_apply = (store.rows.clone(), store.last_history_id);
    let upserts_after_first = store.upserts;
    let deletes_after_first = store.deletes;
    let mailbox_history_after_first = sim.history_id();

    // --- 4. replay exactly the same page ---------------------------------
    // Same bytes, deliberately: this is a redelivered Pub/Sub notification, or a
    // worker that crashed after fetching and before committing.
    let replay = fetch_history_page(&sim, cursor_after_initial);
    assert_eq!(
        replay.body, page.body,
        "the same request against unchanged state must return the same bytes"
    );

    apply_history_page(&sim, &mut store, &replay.json_body());

    // --- 5. nothing changed ----------------------------------------------
    assert_eq!(
        (store.rows.clone(), store.last_history_id),
        state_after_first_apply,
        "the second application of the same page must change nothing"
    );
    assert_eq!(
        sim.history_id(),
        mailbox_history_after_first,
        "reading history must not itself write history"
    );
    assert_eq!(
        store.deletes, deletes_after_first,
        "the delete must not have been counted twice"
    );
    assert!(
        store.upserts > upserts_after_first,
        "the replay really did run — it just had no effect"
    );

    // …and a third fetch, now from the advanced cursor, is empty.
    let empty = fetch_history_page(&sim, store.last_history_id.unwrap()).json_body();
    assert!(empty.get("history").is_none(), "no records left: {empty}");
    assert_eq!(empty["historyId"], sim.history_id().to_string());
}

/// The other half of the same story: the cursor aged out, so history is a `404`
/// and only a full re-sync plus a reconciliation sweep is correct.
#[test]
fn an_expired_cursor_forces_a_full_resync_and_the_sweep_finds_the_drift() {
    let sim = Simulator::new();
    let seeded = Scenario::new()
        .insert(MessageSpec::new().subject("a").received_days_ago(5))
        .insert(MessageSpec::new().subject("b").received_days_ago(4))
        .insert(MessageSpec::new().subject("c").received_days_ago(3))
        .run(&sim)
        .expect("seed");

    let mut store = MiniStore::default();
    initial_sync(&sim, &mut store, "newer_than:30d");
    let stale_cursor = store.last_history_id.unwrap();
    assert_eq!(store.rows.len(), 3);

    // The world moves on while the client is asleep, and the window closes
    // behind it.
    Scenario::new()
        .advance(Duration::from_secs(9 * 24 * 3600))
        .then(nade_gmail_sim::Step::SetRetention(
            HistoryRetention::Records(1),
        ))
        .delete(Target::Id(seeded.inserted[2].clone()))
        .insert(MessageSpec::new().subject("d"))
        .insert(MessageSpec::new().subject("e"))
        .run(&sim)
        .expect("drift");

    // The incremental path is closed.
    let refused = fetch_history_page(&sim, stale_cursor);
    assert_eq!(refused.status, 404, "an aged-out cursor must be a 404");
    let body = refused.json_body();
    assert_eq!(body["error"]["errors"][0]["reason"], "notFound");
    assert_eq!(body["error"]["status"], "NOT_FOUND");

    // Full re-sync into the same store, then a sweep for rows the re-sync did
    // not touch. Without the sweep the deleted message would live forever.
    let before_sweep = store.rows.keys().cloned().collect::<Vec<_>>();
    let mut fresh = MiniStore::default();
    initial_sync(&sim, &mut fresh, "newer_than:30d");
    for id in before_sweep {
        if !fresh.rows.contains_key(&id) {
            store.delete(&id);
        }
    }
    for (id, row) in fresh.rows.clone() {
        store.upsert(&id, row);
    }
    store.last_history_id = fresh.last_history_id;

    assert_eq!(
        store.rows,
        expected_rows(&sim),
        "the sweep must reconcile the store back to the mailbox"
    );
    assert!(
        !store.rows.contains_key(&seeded.inserted[2]),
        "the sweep must find the message deleted while the cursor was stale"
    );
    assert_eq!(store.rows.len(), 4);
    assert_eq!(store.state().1, Some(sim.history_id()));
}

/// A history page whose top-level `historyId` is *not* the right cursor.
///
/// Gmail returns the mailbox's current `historyId` on **every** page. A client
/// that stores it after page one and stops has skipped page two, and the
/// simulator is what makes that visible.
#[test]
fn advancing_to_the_top_level_history_id_after_page_one_loses_records() {
    let sim = Simulator::new();
    let start = sim.history_id();
    for n in 0..6 {
        sim.insert_message(&MessageSpec::new().subject(format!("m{n}")))
            .unwrap();
    }

    let page_one = sim
        .handle(&sim.authorized(&format!(
            "/gmail/v1/users/me/history?startHistoryId={start}&maxResults=2"
        )))
        .json_body();
    assert_eq!(page_one["history"].as_array().map(Vec::len), Some(2));
    assert!(page_one["nextPageToken"].is_string(), "more pages exist");

    let top_level: u64 = page_one["historyId"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        top_level,
        sim.history_id(),
        "the top-level historyId is the mailbox's current one, not this page's last record"
    );

    // The trap: a client that stores `top_level` and comes back sees nothing…
    let naive = sim
        .handle(&sim.authorized(&format!(
            "/gmail/v1/users/me/history?startHistoryId={top_level}"
        )))
        .json_body();
    assert!(
        naive.get("history").is_none(),
        "the four unfetched records are now invisible: {naive}"
    );

    // …whereas following `nextPageToken` gets all six.
    let mut seen = 2usize;
    let mut token = page_one["nextPageToken"].as_str().unwrap().to_owned();
    loop {
        let page = sim
            .handle(&sim.authorized(&format!(
                "/gmail/v1/users/me/history?startHistoryId={start}&maxResults=2&pageToken={token}"
            )))
            .json_body();
        seen += page["history"].as_array().map_or(0, Vec::len);
        match page["nextPageToken"].as_str() {
            Some(next) => token = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(seen, 6, "paging correctly finds every record");
}

/// A message that vanishes between `messages.list` and `messages.get`.
#[test]
fn a_message_deleted_mid_sync_is_a_404_part_and_not_a_failed_sync() {
    let sim = Simulator::new();
    let ids: Vec<String> = (0..4)
        .map(|n| {
            sim.insert_message(&MessageSpec::new().subject(format!("m{n}")))
                .unwrap()
        })
        .collect();

    let listed = sim
        .handle(&sim.authorized("/gmail/v1/users/me/messages"))
        .json_body();
    assert_eq!(listed["messages"].as_array().unwrap().len(), 4);

    // The user deletes one before the client gets round to fetching it.
    sim.delete_message(&ids[1]).unwrap();

    let mut store = MiniStore::default();
    let mut gone = 0;
    for row in listed["messages"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let response =
            sim.handle(&sim.authorized(&format!("/gmail/v1/users/me/messages/{id}?format=raw")));
        if response.status == 404 {
            gone += 1;
            continue;
        }
        store.upsert(id, row_from_message(&response.json_body()));
    }
    assert_eq!(gone, 1);
    assert_eq!(store.rows.len(), 3, "the other three must survive");
    assert_eq!(store.rows, expected_rows(&sim));
}

/// Trash is not delete, and the difference is only visible statefully.
#[test]
fn a_trashed_message_leaves_the_listing_and_stays_gettable() {
    let sim = Simulator::new();
    let id = sim
        .insert_message(&MessageSpec::new().subject("bye"))
        .unwrap();

    sim.trash_message(&id).unwrap();

    let listed = sim
        .handle(&sim.authorized("/gmail/v1/users/me/messages"))
        .json_body();
    assert!(
        listed.get("messages").is_none(),
        "trash is hidden: {listed}"
    );

    let with_trash = sim
        .handle(&sim.authorized("/gmail/v1/users/me/messages?includeSpamTrash=true"))
        .json_body();
    assert_eq!(with_trash["messages"].as_array().unwrap().len(), 1);

    let fetched = sim.handle(&sim.authorized(&format!("/gmail/v1/users/me/messages/{id}")));
    assert_eq!(
        fetched.status, 200,
        "a trashed message is still retrievable — only a deleted one is a 404"
    );
    let labels = labels_of(&fetched.json_body());
    assert!(labels.contains(&"TRASH".to_owned()));
    assert!(!labels.contains(&"INBOX".to_owned()));

    // And the history says label change, never messagesDeleted.
    let page = fetch_history_page(&sim, sim.mailbox(|m| m.history().records()[0].id)).json_body();
    let records = page["history"].as_array().unwrap();
    assert!(records
        .iter()
        .all(|record| record.get("messagesDeleted").is_none()));
    assert!(records
        .iter()
        .any(|record| record.get("labelsAdded").is_some()));

    // Now really delete it.
    sim.delete_message(&id).unwrap();
    assert_eq!(
        sim.handle(&sim.authorized(&format!("/gmail/v1/users/me/messages/{id}")))
            .status,
        404
    );
}

/// The 30-day window a NADE sync uses, at its exact boundary.
#[test]
fn the_thirty_day_window_excludes_a_message_exactly_thirty_days_old() {
    let sim = Simulator::new();
    let now = sim.clock().now_ms();
    let day = 86_400_000i64;

    sim.insert_message(&MessageSpec::new().subject("just inside").received_at(
        nade_gmail_sim::message::ReceivedAt::AtMs(now - 30 * day + 1),
    ))
    .unwrap();
    sim.insert_message(
        &MessageSpec::new()
            .subject("exactly on it")
            .received_at(nade_gmail_sim::message::ReceivedAt::AtMs(now - 30 * day)),
    )
    .unwrap();
    sim.insert_message(
        &MessageSpec::new()
            .subject("outside")
            .received_at(nade_gmail_sim::message::ReceivedAt::AtMs(now - 31 * day)),
    )
    .unwrap();

    let found = sim.mailbox(|mailbox| {
        mailbox
            .search(&Query::parse("newer_than:30d"), now, false, &[])
            .len()
    });
    assert_eq!(found, 1, "only the message strictly inside the window");
}
