//! The history log — the part of Gmail a static stub cannot imitate at all.
//!
//! Every mutation appends exactly one [`HistoryRecord`] and moves the mailbox's
//! `historyId` forward. `users.history.list` then replays those records from a
//! caller-supplied `startHistoryId`, which is how an incremental sync works and
//! therefore where a client's idempotency either holds or does not.
//!
//! Three details are load-bearing and all three are modelled deliberately:
//!
//! * **`historyId` does not increment by one.** Gmail's jumps. A client that
//!   stores `last_history_id + 1` as its next cursor loses changes, and
//!   [`HistoryIdStep::Varying`] — the default — makes that fail here instead of
//!   in production.
//! * **The window is finite.** Records age out, and a `startHistoryId` from
//!   before the retained window is a `404`, not an empty page. That is the
//!   trigger for a full re-sync.
//! * **`historyId` is a string on the wire**, everywhere, including inside
//!   these records.

use std::collections::VecDeque;

use serde_json::{json, Value};

/// The four `historyTypes` Gmail understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HistoryType {
    /// A message appeared in the mailbox.
    MessageAdded,
    /// A message was **permanently** deleted. Moving to Trash is a label change,
    /// not this.
    MessageDeleted,
    /// One or more labels were added to a message.
    LabelAdded,
    /// One or more labels were removed from a message.
    LabelRemoved,
}

impl HistoryType {
    /// All four, in the order Gmail documents them.
    pub const ALL: [Self; 4] = [
        Self::MessageAdded,
        Self::MessageDeleted,
        Self::LabelAdded,
        Self::LabelRemoved,
    ];

    /// The `historyTypes=` query value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MessageAdded => "messageAdded",
            Self::MessageDeleted => "messageDeleted",
            Self::LabelAdded => "labelAdded",
            Self::LabelRemoved => "labelRemoved",
        }
    }

    /// Parse a `historyTypes=` query value, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(value.trim()))
    }
}

/// The message snapshot Gmail embeds in a history record.
///
/// Not a full `Message`: id, thread id, and the labels *as they were after the
/// change*. A client that expects a body here is wrong, and gets to find out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageStub {
    /// The message id.
    pub id: String,
    /// Its thread.
    pub thread_id: String,
    /// Its labels after the change this record describes.
    pub label_ids: Vec<String>,
}

impl MessageStub {
    /// The `{id, threadId, labelIds}` shape used inside `messagesAdded` and
    /// friends.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "threadId": self.thread_id,
            "labelIds": self.label_ids,
        })
    }

    /// The `{id, threadId}` shape used in the record's own `messages` array.
    #[must_use]
    pub fn reference_json(&self) -> Value {
        json!({ "id": self.id, "threadId": self.thread_id })
    }
}

/// A label change on one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelChange {
    /// The message, as it stood after the change.
    pub message: MessageStub,
    /// The labels that were added, or removed.
    pub label_ids: Vec<String>,
}

/// One entry of the history log.
///
/// A single record can carry more than one kind of change: moving a message to
/// Trash adds `TRASH` and removes `INBOX` in one operation, and Gmail reports
/// that as one record with both `labelsAdded` and `labelsRemoved`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryRecord {
    /// The record's own id. Strictly increasing, not contiguous.
    pub id: u64,
    /// Messages that appeared.
    pub messages_added: Vec<MessageStub>,
    /// Messages that were permanently deleted.
    pub messages_deleted: Vec<MessageStub>,
    /// Labels gained.
    pub labels_added: Vec<LabelChange>,
    /// Labels lost.
    pub labels_removed: Vec<LabelChange>,
}

impl HistoryRecord {
    /// Does this record contain a change of the given type?
    #[must_use]
    pub fn has(&self, kind: HistoryType) -> bool {
        match kind {
            HistoryType::MessageAdded => !self.messages_added.is_empty(),
            HistoryType::MessageDeleted => !self.messages_deleted.is_empty(),
            HistoryType::LabelAdded => !self.labels_added.is_empty(),
            HistoryType::LabelRemoved => !self.labels_removed.is_empty(),
        }
    }

    /// Every message this record touches, deduplicated, in first-seen order.
    #[must_use]
    pub fn touched(&self) -> Vec<&MessageStub> {
        let mut out: Vec<&MessageStub> = Vec::new();
        for stub in self
            .messages_added
            .iter()
            .chain(self.messages_deleted.iter())
            .chain(self.labels_added.iter().map(|change| &change.message))
            .chain(self.labels_removed.iter().map(|change| &change.message))
        {
            if !out.iter().any(|seen| seen.id == stub.id) {
                out.push(stub);
            }
        }
        out
    }

    /// Keep only the requested change types, dropping the rest.
    ///
    /// Returns `None` when nothing survives, because Gmail omits a record with
    /// no matching change rather than returning an empty one.
    #[must_use]
    pub fn filtered(&self, types: &[HistoryType]) -> Option<Self> {
        let keep = |kind: HistoryType| types.contains(&kind);
        let filtered = Self {
            id: self.id,
            messages_added: if keep(HistoryType::MessageAdded) {
                self.messages_added.clone()
            } else {
                Vec::new()
            },
            messages_deleted: if keep(HistoryType::MessageDeleted) {
                self.messages_deleted.clone()
            } else {
                Vec::new()
            },
            labels_added: if keep(HistoryType::LabelAdded) {
                self.labels_added.clone()
            } else {
                Vec::new()
            },
            labels_removed: if keep(HistoryType::LabelRemoved) {
                self.labels_removed.clone()
            } else {
                Vec::new()
            },
        };
        HistoryType::ALL
            .into_iter()
            .any(|kind| filtered.has(kind))
            .then_some(filtered)
    }

    /// The wire shape. Absent arrays are **omitted**, not emitted empty — the
    /// difference matters to a client that checks for the key.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut object = serde_json::Map::new();
        // `id` is a string. Always.
        object.insert("id".to_owned(), json!(self.id.to_string()));
        object.insert(
            "messages".to_owned(),
            Value::Array(
                self.touched()
                    .into_iter()
                    .map(MessageStub::reference_json)
                    .collect(),
            ),
        );
        if !self.messages_added.is_empty() {
            object.insert(
                "messagesAdded".to_owned(),
                Value::Array(
                    self.messages_added
                        .iter()
                        .map(|stub| json!({ "message": stub.json() }))
                        .collect(),
                ),
            );
        }
        if !self.messages_deleted.is_empty() {
            object.insert(
                "messagesDeleted".to_owned(),
                Value::Array(
                    self.messages_deleted
                        .iter()
                        .map(|stub| json!({ "message": stub.json() }))
                        .collect(),
                ),
            );
        }
        if !self.labels_added.is_empty() {
            object.insert(
                "labelsAdded".to_owned(),
                Value::Array(self.labels_added.iter().map(change_json).collect()),
            );
        }
        if !self.labels_removed.is_empty() {
            object.insert(
                "labelsRemoved".to_owned(),
                Value::Array(self.labels_removed.iter().map(change_json).collect()),
            );
        }
        Value::Object(object)
    }
}

fn change_json(change: &LabelChange) -> Value {
    json!({ "message": change.message.json(), "labelIds": change.label_ids })
}

/// How far the mailbox's `historyId` jumps per mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryIdStep {
    /// `+1` every time. Convenient, and a lie.
    One,
    /// A deterministic 1/2/3 cycle, so a client that assumes contiguity breaks.
    /// The default.
    Varying,
}

impl HistoryIdStep {
    fn next(self, current: u64, appended: u64) -> u64 {
        match self {
            Self::One => current + 1,
            // 1, 2, 3, 1, 2, 3, … — deterministic, and never contiguous for
            // long enough to let `+1` look correct.
            Self::Varying => current + 1 + appended % 3,
        }
    }
}

/// How much history the mailbox keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRetention {
    /// Never evict. Useful when a test is not about the 404 path.
    Unlimited,
    /// Keep at most this many records; evicting one raises the 404 floor.
    Records(usize),
}

/// The log itself.
#[derive(Debug, Clone)]
pub struct HistoryLog {
    records: VecDeque<HistoryRecord>,
    /// The mailbox's current `historyId`.
    current: u64,
    /// The highest id that has been evicted. A `startHistoryId` below this
    /// cannot be served completely, so it is a 404.
    floor: u64,
    retention: HistoryRetention,
    step: HistoryIdStep,
    appended: u64,
}

impl HistoryLog {
    /// A log whose mailbox starts at `start`, with no records yet.
    #[must_use]
    pub fn new(start: u64) -> Self {
        Self {
            records: VecDeque::new(),
            current: start,
            floor: 0,
            retention: HistoryRetention::Unlimited,
            step: HistoryIdStep::Varying,
            appended: 0,
        }
    }

    /// Change the retention window. Trims immediately.
    pub fn set_retention(&mut self, retention: HistoryRetention) {
        self.retention = retention;
        self.trim();
    }

    /// Change how far `historyId` jumps per mutation.
    pub fn set_step(&mut self, step: HistoryIdStep) {
        self.step = step;
    }

    /// The mailbox's current `historyId`.
    #[must_use]
    pub fn current(&self) -> u64 {
        self.current
    }

    /// The lowest `startHistoryId` that can still be served.
    #[must_use]
    pub fn floor(&self) -> u64 {
        self.floor
    }

    /// Records still retained, oldest first.
    #[must_use]
    pub fn records(&self) -> &VecDeque<HistoryRecord> {
        &self.records
    }

    /// Append a record, assigning it the next `historyId`.
    ///
    /// Returns the id it was given. This is the only way `historyId` ever moves:
    /// there is no code path that bumps it without writing a record, which is
    /// what makes "every mutation is visible to an incremental sync" true by
    /// construction rather than by review.
    pub fn append(&mut self, mut record: HistoryRecord) -> u64 {
        self.current = self.step.next(self.current, self.appended);
        self.appended += 1;
        record.id = self.current;
        self.records.push_back(record);
        self.trim();
        self.current
    }

    fn trim(&mut self) {
        let HistoryRetention::Records(keep) = self.retention else {
            return;
        };
        while self.records.len() > keep {
            if let Some(dropped) = self.records.pop_front() {
                self.floor = self.floor.max(dropped.id);
            }
        }
    }

    /// Force the 404 floor upward without evicting anything, so a test can
    /// reproduce "this cursor is too old" without generating a week of traffic.
    pub fn expire_before(&mut self, history_id: u64) {
        self.floor = self.floor.max(history_id);
        while self
            .records
            .front()
            .is_some_and(|record| record.id < history_id)
        {
            self.records.pop_front();
        }
    }

    /// Can a caller start here, or has the window moved past them?
    #[must_use]
    pub fn can_serve(&self, start_history_id: u64) -> bool {
        start_history_id >= self.floor
    }

    /// Records strictly newer than `start_history_id`, oldest first.
    ///
    /// **Exclusive.** Gmail documents `startHistoryId` as "returns history
    /// records after the specified `startHistoryId`", and a client stores the
    /// `historyId` it last processed, so returning that record again would make
    /// every incremental sync re-apply its final change.
    #[must_use]
    pub fn since(&self, start_history_id: u64) -> Vec<&HistoryRecord> {
        self.records
            .iter()
            .filter(|record| record.id > start_history_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(id: &str) -> MessageStub {
        MessageStub {
            id: id.to_owned(),
            thread_id: id.to_owned(),
            label_ids: vec!["INBOX".to_owned()],
        }
    }

    fn added(id: &str) -> HistoryRecord {
        HistoryRecord {
            messages_added: vec![stub(id)],
            ..HistoryRecord::default()
        }
    }

    #[test]
    fn history_ids_are_strictly_increasing_and_not_contiguous() {
        let mut log = HistoryLog::new(1_000);
        let ids: Vec<u64> = (0..6).map(|n| log.append(added(&n.to_string()))).collect();
        for pair in ids.windows(2) {
            assert!(pair[1] > pair[0], "must increase: {ids:?}");
        }
        assert!(
            ids.windows(2).any(|pair| pair[1] - pair[0] != 1),
            "a client must not be able to assume +1: {ids:?}"
        );
        assert_eq!(log.current(), *ids.last().unwrap());
    }

    #[test]
    fn the_step_can_be_forced_to_one_when_a_test_wants_readable_ids() {
        let mut log = HistoryLog::new(10);
        log.set_step(HistoryIdStep::One);
        assert_eq!(log.append(added("a")), 11);
        assert_eq!(log.append(added("b")), 12);
    }

    #[test]
    fn since_is_exclusive_of_the_start() {
        let mut log = HistoryLog::new(100);
        let first = log.append(added("a"));
        let second = log.append(added("b"));
        assert_eq!(log.since(first).len(), 1);
        assert_eq!(log.since(first)[0].id, second);
        assert!(log.since(second).is_empty(), "no records after the newest");
    }

    #[test]
    fn eviction_raises_the_floor_and_closes_the_window() {
        let mut log = HistoryLog::new(0);
        log.set_retention(HistoryRetention::Records(2));
        let first = log.append(added("a"));
        let second = log.append(added("b"));
        let third = log.append(added("c"));
        assert_eq!(log.records().len(), 2);
        assert_eq!(log.floor(), first);
        assert!(!log.can_serve(first - 1), "before the floor is unservable");
        assert!(log.can_serve(first), "at the floor is fine");
        assert!(log.can_serve(second));
        assert_eq!(log.since(second)[0].id, third);
    }

    #[test]
    fn expire_before_forces_the_window_shut_without_traffic() {
        let mut log = HistoryLog::new(0);
        let first = log.append(added("a"));
        log.append(added("b"));
        log.expire_before(first + 1);
        assert!(!log.can_serve(first));
        assert!(log.can_serve(first + 1));
    }

    #[test]
    fn a_record_carries_every_change_of_one_operation() {
        // Trashing is one mutation: TRASH gained, INBOX lost.
        let record = HistoryRecord {
            id: 7,
            labels_added: vec![LabelChange {
                message: stub("m1"),
                label_ids: vec!["TRASH".to_owned()],
            }],
            labels_removed: vec![LabelChange {
                message: stub("m1"),
                label_ids: vec!["INBOX".to_owned()],
            }],
            ..HistoryRecord::default()
        };
        let body = record.json();
        assert_eq!(body["id"], "7", "historyId is a string");
        assert!(body["id"].is_string());
        assert_eq!(body["labelsAdded"][0]["labelIds"][0], "TRASH");
        assert_eq!(body["labelsRemoved"][0]["labelIds"][0], "INBOX");
        assert_eq!(
            body["messages"].as_array().unwrap().len(),
            1,
            "the same message twice is listed once"
        );
        assert!(body.get("messagesAdded").is_none(), "absent, not empty");
    }

    #[test]
    fn filtering_omits_records_with_nothing_left() {
        let record = HistoryRecord {
            id: 3,
            labels_added: vec![LabelChange {
                message: stub("m"),
                label_ids: vec!["STARRED".to_owned()],
            }],
            ..HistoryRecord::default()
        };
        assert!(record.filtered(&[HistoryType::LabelAdded]).is_some());
        assert!(
            record.filtered(&[HistoryType::MessageAdded]).is_none(),
            "an empty record must be omitted, not returned hollow"
        );
    }

    #[test]
    fn filtering_strips_the_types_that_were_not_asked_for() {
        let record = HistoryRecord {
            id: 3,
            messages_added: vec![stub("m")],
            labels_added: vec![LabelChange {
                message: stub("m"),
                label_ids: vec!["STARRED".to_owned()],
            }],
            ..HistoryRecord::default()
        };
        let only_adds = record.filtered(&[HistoryType::MessageAdded]).unwrap();
        assert!(only_adds.has(HistoryType::MessageAdded));
        assert!(!only_adds.has(HistoryType::LabelAdded));
    }

    #[test]
    fn history_types_parse_case_insensitively_and_reject_junk() {
        assert_eq!(
            HistoryType::parse("messageAdded"),
            Some(HistoryType::MessageAdded)
        );
        assert_eq!(
            HistoryType::parse(" LABELREMOVED "),
            Some(HistoryType::LabelRemoved)
        );
        assert_eq!(HistoryType::parse("messagesAdded"), None);
        assert_eq!(HistoryType::parse(""), None);
    }
}
