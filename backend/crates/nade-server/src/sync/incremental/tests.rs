//! Incremental-walk tests.
//!
//! The pure half needs neither a database nor a server: `plan_page` decides the
//! ordering rules, and those are where the bugs live.

use super::*;

fn page(json: &str) -> HistoryList {
    serde_json::from_str(json).expect("test fixture is valid history JSON")
}

/// Criterion Q1 - all four types on one page, each doing its own thing.
#[test]
fn a_page_plans_every_history_type() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"10","messagesAdded":[{"message":{"id":"a","threadId":"t1"}}]},
             {"id":"11","labelsAdded":[{"message":{"id":"b"},"labelIds":["STARRED"]}]},
             {"id":"12","labelsRemoved":[{"message":{"id":"c"},"labelIds":["UNREAD"]}]},
             {"id":"13","messagesDeleted":[{"message":{"id":"d"}}]}
           ],"historyId":"999"}"#,
    ));

    assert_eq!(plan.to_fetch, vec!["a".to_owned()]);
    assert_eq!(
        plan.label_deltas.get("b"),
        Some(&(vec!["STARRED".to_owned()], vec![]))
    );
    assert_eq!(
        plan.label_deltas.get("c"),
        Some(&(vec![], vec!["UNREAD".to_owned()]))
    );
    assert!(plan.deleted.contains("d"));
    assert_eq!(plan.records, 4);
    assert_eq!(
        plan.cursor,
        Some(13),
        "the cursor is the last record, not 999"
    );
}

/// Criterion Q18 - a label added and then removed on the same page is one net
/// update, not two conflicting ones.
#[test]
fn a_label_added_then_removed_folds_to_a_single_removal() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"1","labelsAdded":[{"message":{"id":"m"},"labelIds":["UNREAD"]}]},
             {"id":"2","labelsRemoved":[{"message":{"id":"m"},"labelIds":["UNREAD"]}]}
           ]}"#,
    ));

    let (added, removed) = plan.label_deltas.get("m").expect("one delta for m");
    assert!(added.is_empty(), "the later record must win: {added:?}");
    assert_eq!(removed, &vec!["UNREAD".to_owned()]);
}

/// The other direction: removed, then added again.
#[test]
fn a_label_removed_then_added_folds_to_a_single_addition() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"1","labelsRemoved":[{"message":{"id":"m"},"labelIds":["INBOX"]}]},
             {"id":"2","labelsAdded":[{"message":{"id":"m"},"labelIds":["INBOX"]}]}
           ]}"#,
    ));

    let (added, removed) = plan.label_deltas.get("m").expect("one delta for m");
    assert_eq!(added, &vec!["INBOX".to_owned()]);
    assert!(removed.is_empty(), "{removed:?}");
}

/// A message added and deleted within one page is never fetched. Fetching it
/// would spend five quota units to store a row we are about to soft-delete.
#[test]
fn a_message_added_and_deleted_on_one_page_is_never_fetched() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"1","messagesAdded":[{"message":{"id":"m","threadId":"t"}}]},
             {"id":"2","messagesDeleted":[{"message":{"id":"m"}}]}
           ]}"#,
    ));

    assert!(plan.to_fetch.is_empty(), "{:?}", plan.to_fetch);
    assert!(plan.deleted.contains("m"));
    assert!(
        plan.label_deltas.is_empty(),
        "a deleted message needs no label work"
    );
}

/// ...and a message deleted and then re-added is fetched, because the last
/// record is the one that describes the world.
#[test]
fn a_message_deleted_then_added_again_is_fetched() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"1","messagesDeleted":[{"message":{"id":"m"}}]},
             {"id":"2","messagesAdded":[{"message":{"id":"m","threadId":"t"}}]}
           ]}"#,
    ));

    assert_eq!(plan.to_fetch, vec!["m".to_owned()]);
    assert!(!plan.deleted.contains("m"));
}

/// Criterion Q7 - the rule the whole walk turns on, stated as a test.
///
/// `historyId` is the mailbox's current id and Gmail repeats it on every page.
/// Storing it after page one asks the next walk to start past everything that
/// had not been read, and the answer is a `200` with no `history` key.
#[test]
fn the_cursor_is_the_last_record_id_and_never_the_top_level_history_id() {
    let first_of_several = page(
        r#"{"history":[{"id":"101"},{"id":"102"}],
            "nextPageToken":"tok","historyId":"999"}"#,
    );
    let plan = plan_page(&first_of_several);

    assert_eq!(plan.cursor, Some(102));
    assert_ne!(
        plan.cursor,
        Some(999),
        "storing the top-level historyId after page one loses every later page"
    );
}

/// Criterion Q11 - an empty page has no cursor at all, so nothing moves.
#[test]
fn an_empty_page_plans_nothing_and_offers_no_cursor() {
    let plan = plan_page(&page(r#"{"historyId":"999"}"#));

    assert_eq!(plan, PagePlan::default());
    assert_eq!(plan.cursor, None);
}

/// Gmail does not promise an order, and the fold is only correct in one.
/// Presented backwards, the page must still resolve to "removed".
#[test]
fn records_are_folded_in_id_order_not_arrival_order() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"2","labelsRemoved":[{"message":{"id":"m"},"labelIds":["UNREAD"]}]},
             {"id":"1","labelsAdded":[{"message":{"id":"m"},"labelIds":["UNREAD"]}]}
           ]}"#,
    ));

    let (added, removed) = plan.label_deltas.get("m").expect("one delta");
    assert!(added.is_empty(), "arrival order won instead of id order");
    assert_eq!(removed, &vec!["UNREAD".to_owned()]);
    assert_eq!(plan.cursor, Some(2));
}

/// Criterion Q4 - trash arrives as a label change, never as `messagesDeleted`.
/// This is the shape `nade-gmail-sim`'s story test asserts Gmail really sends.
#[test]
fn trashing_is_planned_as_a_label_move_not_a_deletion() {
    let plan = plan_page(&page(
        r#"{"history":[{"id":"5",
             "labelsRemoved":[{"message":{"id":"m"},"labelIds":["INBOX"]}],
             "labelsAdded":[{"message":{"id":"m"},"labelIds":["TRASH"]}]}]}"#,
    ));

    assert!(plan.deleted.is_empty(), "a trashed message is not deleted");
    let (added, removed) = plan.label_deltas.get("m").expect("one delta");
    assert_eq!(added, &vec!["TRASH".to_owned()]);
    assert_eq!(removed, &vec!["INBOX".to_owned()]);
}

/// EDGE (empty input): a record with no message id is **counted**, not
/// silently dropped.
///
/// Gmail has never sent one; a schema change or a corrupt response would. The
/// count becomes a `history_record_malformed` audit row inside the page
/// transaction, so the anomaly leaves evidence.
///
/// It deliberately does **not** block the cursor. The id is the only handle on
/// the message, so refusing to advance would re-read the same malformed record
/// forever - wedging the account without recovering anything - while the
/// 30-day sync and the reconciliation sweep can still find the message. A
/// wedge that also loses the mail is strictly worse than an audited skip.
#[test]
fn records_with_no_message_id_are_counted_not_silently_dropped() {
    let plan = plan_page(&page(
        r#"{"history":[{"id":"1",
             "messagesAdded":[{"message":{"id":""}}],
             "messagesDeleted":[{"message":{"id":""}}],
             "labelsAdded":[{"message":{"id":""},"labelIds":["X"]}]}]}"#,
    ));

    assert!(plan.to_fetch.is_empty());
    assert!(plan.label_deltas.is_empty());
    assert!(plan.deleted.is_empty());
    assert_eq!(plan.malformed, 3, "every malformed entry must be counted");
    assert_eq!(plan.cursor, Some(1), "the page still happened");
}

/// A well-formed page reports no anomalies, so `malformed` cannot be a counter
/// that is always non-zero.
#[test]
fn a_well_formed_page_reports_no_malformed_records() {
    let plan = plan_page(&page(
        r#"{"history":[{"id":"1","messagesAdded":[{"message":{"id":"m","threadId":"t"}}]}]}"#,
    ));
    assert_eq!(plan.malformed, 0);
}

/// One message named twice by two `messagesAdded` records is fetched once.
#[test]
fn a_message_added_twice_is_fetched_once() {
    let plan = plan_page(&page(
        r#"{"history":[
             {"id":"1","messagesAdded":[{"message":{"id":"m","threadId":"t"}}]},
             {"id":"2","messagesAdded":[{"message":{"id":"m","threadId":"t"}}]}
           ]}"#,
    ));

    assert_eq!(plan.to_fetch, vec!["m".to_owned()]);
}
