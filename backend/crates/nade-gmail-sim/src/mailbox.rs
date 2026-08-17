//! The mailbox: messages, labels, threads, and the history log that binds them.
//!
//! Every mutating method here does the same three things, in the same order:
//! change the state, build one [`HistoryRecord`], append it. There is no path
//! that changes state without writing history and none that writes history
//! without changing state, so "an incremental sync sees everything an initial
//! sync would" is true by construction.
//!
//! A mutation that would change nothing — adding a label a message already has,
//! removing one it does not — returns `Ok(None)` and writes **no** record.
//! Gmail behaves the same way, and it matters: a client that replays a history
//! page must not manufacture new history by doing so.

use std::collections::{BTreeMap, HashMap};

use crate::{
    error::{Result, SimError},
    history::{HistoryLog, HistoryRecord, LabelChange, MessageStub},
    ids::{hex16, MESSAGE_ID_BASE},
    label::{self, Label, LabelCounts, LabelType, SYSTEM_LABELS},
    message::{self, MessageSpec, ReceivedAt, StoredMessage, ThreadPlacement},
    mime::MimePart,
    query::{decode_rfc2047, Facts, Query, Searchable},
};

/// The `historyId` a fresh mailbox starts at.
///
/// Non-zero on purpose: a real mailbox's first `historyId` is a large number,
/// and a client that treats `0` as "no cursor yet" must not be handed a `0`.
pub const INITIAL_HISTORY_ID: u64 = 1_000_000;

/// A whole Gmail mailbox.
#[derive(Debug, Clone)]
pub struct Mailbox {
    /// The account's address, returned by `getProfile`.
    pub email_address: String,
    messages: BTreeMap<String, StoredMessage>,
    index: HashMap<String, Searchable>,
    labels: Vec<Label>,
    history: HistoryLog,
    next_message_seq: u64,
    next_label_seq: u64,
}

impl Mailbox {
    /// An empty mailbox with Gmail's fourteen system labels and nothing else.
    #[must_use]
    pub fn new(email_address: impl Into<String>) -> Self {
        Self {
            email_address: email_address.into(),
            messages: BTreeMap::new(),
            index: HashMap::new(),
            labels: SYSTEM_LABELS.iter().map(|id| Label::system(id)).collect(),
            history: HistoryLog::new(INITIAL_HISTORY_ID),
            next_message_seq: 0,
            next_label_seq: 1,
        }
    }

    // -- reads ------------------------------------------------------------

    /// The mailbox's current `historyId`.
    #[must_use]
    pub fn history_id(&self) -> u64 {
        self.history.current()
    }

    /// The history log, for `users.history.list` and for a test's assertions.
    #[must_use]
    pub fn history(&self) -> &HistoryLog {
        &self.history
    }

    /// The history log, mutably, so a test can shrink the retention window or
    /// force a cursor to expire.
    pub fn history_mut(&mut self) -> &mut HistoryLog {
        &mut self.history
    }

    /// One message, if it exists. A trashed message **does** exist.
    #[must_use]
    pub fn message(&self, id: &str) -> Option<&StoredMessage> {
        self.messages.get(id)
    }

    /// Every message, newest first — `messages.list`'s order.
    ///
    /// Ties on `internalDate` break on id, descending, so the order is total.
    /// Without that, two messages received in the same millisecond could swap
    /// places between two pages of one listing and a keyset cursor would either
    /// skip or repeat one.
    #[must_use]
    pub fn messages_newest_first(&self) -> Vec<&StoredMessage> {
        let mut all: Vec<&StoredMessage> = self.messages.values().collect();
        all.sort_by(|a, b| {
            b.internal_date_ms
                .cmp(&a.internal_date_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        all
    }

    /// How many messages the mailbox holds, trash and spam included — which is
    /// what `getProfile.messagesTotal` counts.
    #[must_use]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Distinct thread ids.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        let mut seen: Vec<&str> = self
            .messages
            .values()
            .map(|message| message.thread_id.as_str())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }

    /// The messages of one thread, oldest first — Gmail's thread order.
    #[must_use]
    pub fn thread(&self, thread_id: &str) -> Vec<&StoredMessage> {
        let mut all: Vec<&StoredMessage> = self
            .messages
            .values()
            .filter(|message| message.thread_id == thread_id)
            .collect();
        all.sort_by(|a, b| {
            a.internal_date_ms
                .cmp(&b.internal_date_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        all
    }

    /// Thread ids, newest thread first, where a thread's age is its newest
    /// message.
    #[must_use]
    pub fn threads_newest_first(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for message in self.messages_newest_first() {
            if !seen.contains(&message.thread_id) {
                seen.push(message.thread_id.clone());
            }
        }
        seen
    }

    /// Every label, system first in Gmail's order, then user labels by creation.
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    /// One label by id.
    #[must_use]
    pub fn label(&self, id: &str) -> Option<&Label> {
        self.labels.iter().find(|label| label.id == id)
    }

    /// A label's display name, for `label:` queries.
    #[must_use]
    pub fn label_name(&self, id: &str) -> Option<String> {
        self.label(id).map(|label| label.name.clone())
    }

    /// The counters `labels.get` returns and `labels.list` does not.
    #[must_use]
    pub fn label_counts(&self, id: &str) -> LabelCounts {
        let mut counts = LabelCounts::default();
        let mut threads: Vec<&str> = Vec::new();
        let mut unread_threads: Vec<&str> = Vec::new();
        for message in self.messages.values() {
            if !message.has_label(id) {
                continue;
            }
            counts.messages_total += 1;
            let unread = message.has_label("UNREAD");
            if unread {
                counts.messages_unread += 1;
            }
            if !threads.contains(&message.thread_id.as_str()) {
                threads.push(&message.thread_id);
            }
            if unread && !unread_threads.contains(&message.thread_id.as_str()) {
                unread_threads.push(&message.thread_id);
            }
        }
        counts.threads_total = threads.len() as u64;
        counts.threads_unread = unread_threads.len() as u64;
        counts
    }

    /// Run a parsed query.
    ///
    /// `include_spam_trash` is `messages.list`'s parameter: without it, messages
    /// carrying `SPAM` or `TRASH` are invisible. That, and not a `404` from
    /// `messages.get`, is how a trashed message "disappears".
    #[must_use]
    pub fn search(
        &self,
        query: &Query,
        now_ms: i64,
        include_spam_trash: bool,
        label_filter: &[String],
    ) -> Vec<&StoredMessage> {
        let name_of = |id: &str| self.label_name(id);
        self.messages_newest_first()
            .into_iter()
            .filter(|message| {
                if !include_spam_trash
                    && label::HIDDEN_WITHOUT_SPAM_TRASH
                        .iter()
                        .any(|hidden| message.has_label(hidden))
                {
                    return false;
                }
                if !label_filter.iter().all(|wanted| message.has_label(wanted)) {
                    return false;
                }
                let empty = Searchable::default();
                let text = self.index.get(&message.id).unwrap_or(&empty);
                query.matches(
                    text,
                    Facts {
                        internal_date_ms: message.internal_date_ms,
                        label_ids: &message.label_ids,
                        size: message.size_estimate(),
                        now_ms,
                    },
                    &name_of,
                )
            })
            .collect()
    }

    // -- mutations --------------------------------------------------------

    /// Insert a message. Appends one `messagesAdded` record.
    ///
    /// Returns the new message's id. `now_ms` comes from the simulator's clock;
    /// the mailbox itself never reads one.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] when [`ThreadPlacement::ReplyTo`] names a
    /// message that is not here, and [`SimError::InvalidSpec`] when a forced id
    /// is already taken — silently overwriting would make an "insert" quietly a
    /// "replace", and the history record would lie about which it was.
    pub fn insert_message(&mut self, spec: &MessageSpec, now_ms: i64) -> Result<String> {
        let seq = self.next_message_seq;
        let id = match &spec.forced_id {
            Some(forced) => forced.clone(),
            None => hex16(MESSAGE_ID_BASE, seq),
        };
        if self.messages.contains_key(&id) {
            return Err(SimError::InvalidSpec(format!(
                "message id {id} is already in the mailbox"
            )));
        }
        self.next_message_seq += 1;

        let internal_date_ms = match spec.received_at {
            ReceivedAt::Now => now_ms,
            ReceivedAt::AtMs(at) => at,
            ReceivedAt::OffsetMs(by) => now_ms.saturating_add(by),
        };
        let thread_id = match &spec.thread {
            ThreadPlacement::NewThread => id.clone(),
            ThreadPlacement::ThreadId(thread) => thread.clone(),
            ThreadPlacement::ReplyTo(other) => self
                .messages
                .get(other)
                .map(|message| message.thread_id.clone())
                .ok_or_else(|| SimError::NoSuchMessage(other.clone()))?,
        };

        let raw = spec.to_raw_at(seq, internal_date_ms);
        let message = StoredMessage {
            id: id.clone(),
            thread_id,
            label_ids: spec.labels.clone(),
            internal_date_ms,
            raw,
            history_id: 0,
        };
        let searchable = build_index(&message);
        self.index.insert(id.clone(), searchable);
        self.messages.insert(id.clone(), message);

        let stub = self.stub(&id);
        let history_id = self.history.append(HistoryRecord {
            messages_added: vec![stub],
            ..HistoryRecord::default()
        });
        if let Some(message) = self.messages.get_mut(&id) {
            message.history_id = history_id;
        }
        Ok(id)
    }

    /// Permanently delete a message — `users.messages.delete`, not Trash.
    ///
    /// Appends one `messagesDeleted` record. Returns the new `historyId`, or
    /// `None` when the message was not there (a second delete is a no-op, not an
    /// error, and must not move history).
    ///
    /// # Errors
    /// Never; the signature carries `Result` only so callers can treat it like
    /// the other mutations.
    pub fn delete_message(&mut self, id: &str) -> Result<Option<u64>> {
        let Some(message) = self.messages.remove(id) else {
            return Ok(None);
        };
        self.index.remove(id);
        let stub = MessageStub {
            id: message.id.clone(),
            thread_id: message.thread_id.clone(),
            label_ids: message.label_ids.clone(),
        };
        Ok(Some(self.history.append(HistoryRecord {
            messages_deleted: vec![stub],
            ..HistoryRecord::default()
        })))
    }

    /// Move a message to Trash — `users.messages.trash`.
    ///
    /// Adds `TRASH`, removes `INBOX` and `UNREAD`, in **one** history record, so
    /// a client that assumes one change per record breaks here. The message is
    /// still returned by `messages.get`; only `messages.list` hides it. That is
    /// Gmail's actual behaviour and the single most-assumed-wrong thing about
    /// Trash.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn trash_message(&mut self, id: &str) -> Result<Option<u64>> {
        self.modify(id, &["TRASH"], &["INBOX", "UNREAD"])
    }

    /// Take a message out of Trash and put it back in the inbox.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn untrash_message(&mut self, id: &str) -> Result<Option<u64>> {
        self.modify(id, &["INBOX"], &["TRASH"])
    }

    /// Add one label.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn add_label(&mut self, id: &str, label_id: &str) -> Result<Option<u64>> {
        self.modify(id, &[label_id], &[])
    }

    /// Remove one label. A second removal is a no-op and writes no history.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn remove_label(&mut self, id: &str, label_id: &str) -> Result<Option<u64>> {
        self.modify(id, &[], &[label_id])
    }

    /// Drop `UNREAD`.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn mark_read(&mut self, id: &str) -> Result<Option<u64>> {
        self.modify(id, &[], &["UNREAD"])
    }

    /// Add `UNREAD`.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn mark_unread(&mut self, id: &str) -> Result<Option<u64>> {
        self.modify(id, &["UNREAD"], &[])
    }

    /// `users.messages.modify`: add and remove labels in one operation, which
    /// is one history record.
    ///
    /// Returns `Ok(None)` when nothing actually changed.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if the id is unknown.
    pub fn modify(&mut self, id: &str, add: &[&str], remove: &[&str]) -> Result<Option<u64>> {
        self.bulk_modify(std::slice::from_ref(&id), add, remove)
    }

    /// Modify many messages as **one** history record.
    ///
    /// This exists because Gmail really does emit records that touch dozens of
    /// messages — "mark all as read" is one record with dozens of
    /// `labelsRemoved` entries — and a client that reads `history[0].messages[0]`
    /// and stops is wrong in a way only a multi-message record can show.
    ///
    /// # Errors
    /// [`SimError::NoSuchMessage`] if any id is unknown; nothing is changed in
    /// that case.
    pub fn bulk_modify(
        &mut self,
        ids: &[&str],
        add: &[&str],
        remove: &[&str],
    ) -> Result<Option<u64>> {
        for id in ids {
            if !self.messages.contains_key(*id) {
                return Err(SimError::NoSuchMessage((*id).to_owned()));
            }
        }

        let mut added: Vec<(String, Vec<String>)> = Vec::new();
        let mut removed: Vec<(String, Vec<String>)> = Vec::new();
        for id in ids {
            let Some(message) = self.messages.get_mut(*id) else {
                continue;
            };
            let gained: Vec<String> = add
                .iter()
                .filter(|label| message.add_label(label))
                .map(|label| (*label).to_owned())
                .collect();
            let lost: Vec<String> = remove
                .iter()
                .filter(|label| message.remove_label(label))
                .map(|label| (*label).to_owned())
                .collect();
            if !gained.is_empty() {
                added.push(((*id).to_owned(), gained));
            }
            if !lost.is_empty() {
                removed.push(((*id).to_owned(), lost));
            }
        }

        // EDGE: a label removed twice, or added when already present. Nothing
        // changed, so nothing is recorded and `historyId` does not move. A
        // simulator that bumped it anyway would make every replayed history page
        // look like new work.
        if added.is_empty() && removed.is_empty() {
            return Ok(None);
        }

        let record = HistoryRecord {
            labels_added: added
                .into_iter()
                .map(|(id, labels)| LabelChange {
                    message: self.stub(&id),
                    label_ids: labels,
                })
                .collect(),
            labels_removed: removed
                .into_iter()
                .map(|(id, labels)| LabelChange {
                    message: self.stub(&id),
                    label_ids: labels,
                })
                .collect(),
            ..HistoryRecord::default()
        };
        let history_id = self.history.append(record);
        for id in ids {
            if let Some(message) = self.messages.get_mut(*id) {
                message.history_id = history_id;
            }
        }
        Ok(Some(history_id))
    }

    /// Create a user label. Ids look like `Label_3`.
    ///
    /// # Errors
    /// [`SimError::DuplicateLabel`] when the name is taken — Gmail rejects that
    /// with a `409`, and letting two labels share a name would make `label:`
    /// queries ambiguous.
    pub fn create_label(&mut self, name: &str) -> Result<String> {
        if self
            .labels
            .iter()
            .any(|label| label.name.eq_ignore_ascii_case(name))
        {
            return Err(SimError::DuplicateLabel(name.to_owned()));
        }
        let id = format!("Label_{}", self.next_label_seq);
        self.next_label_seq += 1;
        self.labels.push(Label::user(&id, name));
        Ok(id)
    }

    /// Delete a user label, taking it off every message it was on.
    ///
    /// The removals are **one** history record with an entry per message.
    ///
    /// # Errors
    /// [`SimError::NoSuchLabel`] or [`SimError::SystemLabelIsImmutable`].
    pub fn delete_label(&mut self, id: &str) -> Result<Option<u64>> {
        if label::is_system(id) {
            return Err(SimError::SystemLabelIsImmutable(id.to_owned()));
        }
        if !self.labels.iter().any(|label| label.id == id) {
            return Err(SimError::NoSuchLabel(id.to_owned()));
        }
        self.labels.retain(|label| label.id != id);
        let carriers: Vec<String> = self
            .messages
            .values()
            .filter(|message| message.has_label(id))
            .map(|message| message.id.clone())
            .collect();
        if carriers.is_empty() {
            return Ok(None);
        }
        let borrowed: Vec<&str> = carriers.iter().map(String::as_str).collect();
        self.bulk_modify(&borrowed, &[], &[id])
    }

    fn stub(&self, id: &str) -> MessageStub {
        self.messages.get(id).map_or_else(
            || MessageStub {
                id: id.to_owned(),
                thread_id: id.to_owned(),
                label_ids: Vec::new(),
            },
            |message| MessageStub {
                id: message.id.clone(),
                thread_id: message.thread_id.clone(),
                label_ids: message.label_ids.clone(),
            },
        )
    }
}

/// Build the search index for one message.
///
/// Runs once, at insert time, from the stored bytes — so what a query sees can
/// never drift from what `format=raw` returns.
fn build_index(message: &StoredMessage) -> Searchable {
    let tree = message.parts();
    let header = |name: &str| {
        tree.header(name)
            .map(decode_rfc2047)
            .unwrap_or_default()
            .to_lowercase()
    };

    let mut body = String::new();
    let mut filenames = Vec::new();
    let mut has_attachment = false;
    for part in tree.walk() {
        if part.is_attachment {
            has_attachment = true;
            if !part.filename.is_empty() {
                filenames.push(decode_rfc2047(&part.filename).to_lowercase());
            }
            continue;
        }
        match part.mime_type.as_str() {
            "text/plain" => body.push_str(&decode_body(part)),
            "text/html" => body.push_str(&strip_tags(&decode_body(part))),
            _ => {}
        }
        body.push(' ');
    }

    Searchable {
        subject: header("subject"),
        from: header("from"),
        to: header("to"),
        cc: header("cc"),
        bcc: header("bcc"),
        list: format!("{} {}", header("list-id"), header("list-unsubscribe")),
        rfc822_msgid: header("message-id").trim_matches(['<', '>']).to_owned(),
        filenames,
        body: body.split_whitespace().collect::<Vec<_>>().join(" "),
        has_attachment,
    }
}

/// Decode a part's bytes to text, honouring the declared charset well enough for
/// an index.
fn decode_body(part: &MimePart) -> String {
    let charset = part
        .header("content-type")
        .map(|value| crate::mime::parse_parameterised(value).1)
        .and_then(|params| {
            params
                .into_iter()
                .find(|(name, _)| name == "charset")
                .map(|(_, value)| value.to_ascii_lowercase())
        })
        .unwrap_or_else(|| "utf-8".to_owned());

    if charset.starts_with("utf") || charset.is_empty() {
        String::from_utf8_lossy(&part.body).to_lowercase()
    } else {
        // windows-1252 for anything else: `docs/PARSER.md` measured that
        // senders declaring `iso-8859-1` overwhelmingly mean cp1252.
        part.body
            .iter()
            .map(|byte| cp1252_char(*byte))
            .collect::<String>()
            .to_lowercase()
    }
}

fn cp1252_char(byte: u8) -> char {
    // Reuse the query module's table by round-tripping through a single-byte
    // encoded word would be silly; this is the same 32-entry block.
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}',
        '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}',
        '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    if (0x80..0xA0).contains(&byte) {
        HIGH[usize::from(byte - 0x80)]
    } else {
        char::from(byte)
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for character in html.chars() {
        match character {
            '<' => inside = true,
            '>' => {
                inside = false;
                out.push(' ');
            }
            other if !inside => out.push(other),
            _ => {}
        }
    }
    out
}

/// The type of a label, for callers that want to filter without matching on the
/// whole [`Label`].
#[must_use]
pub fn kind_of(label: &Label) -> LabelType {
    label.kind
}

/// A message's `internalDate` from its `Date` header, or `None`.
///
/// Used when seeding real `.eml` files: their receipt time is not recorded
/// anywhere else, and inventing one would put every seeded message at the same
/// instant and make `messages.list` order meaningless.
#[must_use]
pub fn date_header_ms(raw: &[u8]) -> Option<i64> {
    let (head, _) = crate::mime::split_header_body(raw);
    crate::mime::parse_headers(head)
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("date"))
        .and_then(|(_, value)| message::parse_rfc2822(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clock::DAY_MS, history::HistoryType};

    const NOW: i64 = 1_785_542_400_000;

    fn mailbox() -> Mailbox {
        Mailbox::new("me@example.com")
    }

    fn insert(box_: &mut Mailbox, subject: &str) -> String {
        box_.insert_message(&MessageSpec::new().subject(subject).text("body"), NOW)
            .unwrap()
    }

    #[test]
    fn a_fresh_mailbox_has_fourteen_system_labels_and_nothing_else() {
        let box_ = mailbox();
        assert_eq!(box_.labels().len(), 14);
        assert!(box_.labels().iter().all(|l| l.kind == LabelType::System));
        assert_eq!(box_.message_count(), 0);
        assert_eq!(box_.thread_count(), 0);
        assert_eq!(box_.history_id(), INITIAL_HISTORY_ID);
        assert!(box_.history().records().is_empty());
    }

    #[test]
    fn every_mutation_appends_exactly_one_record_and_moves_history() {
        let mut box_ = mailbox();
        let mut previous = box_.history_id();
        let mut expected = 0usize;

        let id = insert(&mut box_, "one");
        expected += 1;
        assert_eq!(box_.history().records().len(), expected);
        assert!(box_.history_id() > previous);
        previous = box_.history_id();

        for step in [
            Mutation::AddLabel("STARRED"),
            Mutation::RemoveLabel("UNREAD"),
            Mutation::Trash,
            Mutation::Untrash,
            Mutation::Delete,
        ] {
            step.apply(&mut box_, &id);
            expected += 1;
            assert_eq!(
                box_.history().records().len(),
                expected,
                "{step:?} must append exactly one record"
            );
            assert!(box_.history_id() > previous, "{step:?} must move historyId");
            previous = box_.history_id();
        }
    }

    #[derive(Debug)]
    enum Mutation {
        AddLabel(&'static str),
        RemoveLabel(&'static str),
        Trash,
        Untrash,
        Delete,
    }

    impl Mutation {
        fn apply(&self, box_: &mut Mailbox, id: &str) {
            let _ = match self {
                Self::AddLabel(label) => box_.add_label(id, label).unwrap(),
                Self::RemoveLabel(label) => box_.remove_label(id, label).unwrap(),
                Self::Trash => box_.trash_message(id).unwrap(),
                Self::Untrash => box_.untrash_message(id).unwrap(),
                Self::Delete => box_.delete_message(id).unwrap(),
            };
        }
    }

    #[test]
    fn a_no_op_mutation_writes_no_history() {
        let mut box_ = mailbox();
        let id = insert(&mut box_, "one");
        let before = box_.history_id();
        let records = box_.history().records().len();

        assert_eq!(box_.add_label(&id, "INBOX").unwrap(), None, "already there");
        assert!(box_.remove_label(&id, "UNREAD").unwrap().is_some());
        assert_eq!(
            box_.remove_label(&id, "UNREAD").unwrap(),
            None,
            "a label removed twice is one change, not two"
        );
        assert_eq!(box_.mark_read(&id).unwrap(), None, "already read");

        assert_eq!(box_.history().records().len(), records + 1);
        assert!(box_.history_id() > before);
    }

    #[test]
    fn deleting_a_message_twice_writes_one_record() {
        let mut box_ = mailbox();
        let id = insert(&mut box_, "one");
        assert!(box_.delete_message(&id).unwrap().is_some());
        let after = box_.history_id();
        assert_eq!(box_.delete_message(&id).unwrap(), None);
        assert_eq!(box_.history_id(), after);
    }

    #[test]
    fn trashing_is_one_record_with_both_kinds_of_change() {
        let mut box_ = mailbox();
        let id = insert(&mut box_, "one");
        let before = box_.history().records().len();
        box_.trash_message(&id).unwrap();
        assert_eq!(box_.history().records().len(), before + 1);

        let record = box_.history().records().back().unwrap();
        assert!(record.has(HistoryType::LabelAdded));
        assert!(record.has(HistoryType::LabelRemoved));
        assert!(
            !record.has(HistoryType::MessageDeleted),
            "trash is not delete"
        );

        let message = box_.message(&id).expect("a trashed message still exists");
        assert!(message.has_label("TRASH"));
        assert!(!message.has_label("INBOX"));
    }

    #[test]
    fn a_trashed_message_leaves_the_listing_but_not_the_mailbox() {
        let mut box_ = mailbox();
        let id = insert(&mut box_, "one");
        box_.trash_message(&id).unwrap();
        let all = Query::All;
        assert!(box_.search(&all, NOW, false, &[]).is_empty());
        assert_eq!(box_.search(&all, NOW, true, &[]).len(), 1);
        assert!(box_.message(&id).is_some());
    }

    #[test]
    fn a_bulk_modify_is_one_record_touching_many_messages() {
        let mut box_ = mailbox();
        let ids: Vec<String> = (0..5).map(|n| insert(&mut box_, &n.to_string())).collect();
        let before = box_.history().records().len();
        let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
        box_.bulk_modify(&borrowed, &[], &["UNREAD"]).unwrap();

        assert_eq!(box_.history().records().len(), before + 1);
        let record = box_.history().records().back().unwrap();
        assert_eq!(record.labels_removed.len(), 5);
        assert_eq!(record.touched().len(), 5);
    }

    #[test]
    fn bulk_modify_with_an_unknown_id_changes_nothing() {
        let mut box_ = mailbox();
        let id = insert(&mut box_, "one");
        let before = box_.history_id();
        assert!(box_.bulk_modify(&[&id, "nope"], &[], &["UNREAD"]).is_err());
        assert!(box_.message(&id).unwrap().has_label("UNREAD"));
        assert_eq!(box_.history_id(), before);
    }

    #[test]
    fn message_history_id_tracks_the_last_change_to_it() {
        let mut box_ = mailbox();
        let first = insert(&mut box_, "one");
        let after_insert = box_.message(&first).unwrap().history_id;
        let second = insert(&mut box_, "two");
        assert_eq!(
            box_.message(&first).unwrap().history_id,
            after_insert,
            "an unrelated insert must not move this message's historyId"
        );
        box_.add_label(&first, "STARRED").unwrap();
        assert!(box_.message(&first).unwrap().history_id > after_insert);
        assert_ne!(second, first);
    }

    #[test]
    fn listing_order_is_newest_first_with_a_total_tiebreak() {
        let mut box_ = mailbox();
        // Three messages in the same millisecond, plus one older.
        let same: Vec<String> = (0..3)
            .map(|n| {
                box_.insert_message(
                    &MessageSpec::new()
                        .subject(n.to_string())
                        .received_at(ReceivedAt::AtMs(NOW)),
                    NOW,
                )
                .unwrap()
            })
            .collect();
        let older = box_
            .insert_message(
                &MessageSpec::new().received_at(ReceivedAt::AtMs(NOW - DAY_MS)),
                NOW,
            )
            .unwrap();

        let order: Vec<&str> = box_
            .messages_newest_first()
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(order.last(), Some(&older.as_str()));
        // Ids descend within the tie, so the order is total and repeatable.
        let mut expected: Vec<&str> = same.iter().map(String::as_str).collect();
        expected.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(&order[..3], &expected[..]);
        assert_eq!(box_.messages_newest_first(), box_.messages_newest_first());
    }

    #[test]
    fn replies_join_the_thread_and_the_count_follows() {
        let mut box_ = mailbox();
        let root = insert(&mut box_, "first");
        assert_eq!(box_.thread_count(), 1);
        let reply = box_
            .insert_message(
                &MessageSpec::new().subject("Re: first").reply_to(&root),
                NOW,
            )
            .unwrap();
        assert_eq!(box_.thread_count(), 1, "a reply does not make a new thread");
        assert_eq!(box_.message_count(), 2);
        assert_eq!(box_.thread(&root).len(), 2);
        assert_eq!(box_.thread(&root)[0].id, root, "threads read oldest first");
        assert_eq!(box_.thread(&root)[1].id, reply);
    }

    #[test]
    fn replying_to_a_message_that_is_not_here_is_an_error() {
        let mut box_ = mailbox();
        let outcome = box_.insert_message(&MessageSpec::new().reply_to("nope"), NOW);
        assert!(matches!(outcome, Err(SimError::NoSuchMessage(_))));
        assert_eq!(box_.message_count(), 0, "nothing may be half-inserted");
        assert_eq!(box_.history_id(), INITIAL_HISTORY_ID);
    }

    #[test]
    fn a_forced_id_that_collides_is_rejected() {
        let mut box_ = mailbox();
        box_.insert_message(&MessageSpec::new().id("dup"), NOW)
            .unwrap();
        let again = box_.insert_message(&MessageSpec::new().id("dup"), NOW);
        assert!(matches!(again, Err(SimError::InvalidSpec(_))));
        assert_eq!(box_.message_count(), 1);
    }

    #[test]
    fn user_labels_get_gmail_shaped_ids_and_cannot_collide() {
        let mut box_ = mailbox();
        assert_eq!(box_.create_label("Receipts").unwrap(), "Label_1");
        assert_eq!(box_.create_label("Work").unwrap(), "Label_2");
        assert!(matches!(
            box_.create_label("receipts"),
            Err(SimError::DuplicateLabel(_))
        ));
        assert_eq!(box_.labels().len(), 16);
    }

    #[test]
    fn deleting_a_label_strips_it_everywhere_in_one_record() {
        let mut box_ = mailbox();
        let label = box_.create_label("Receipts").unwrap();
        let ids: Vec<String> = (0..3).map(|n| insert(&mut box_, &n.to_string())).collect();
        for id in &ids {
            box_.add_label(id, &label).unwrap();
        }
        let before = box_.history().records().len();
        box_.delete_label(&label).unwrap();

        assert_eq!(box_.history().records().len(), before + 1);
        assert_eq!(
            box_.history()
                .records()
                .back()
                .unwrap()
                .labels_removed
                .len(),
            3
        );
        assert!(box_.label(&label).is_none());
        assert!(ids
            .iter()
            .all(|id| !box_.message(id).unwrap().has_label(&label)));
    }

    #[test]
    fn system_labels_cannot_be_deleted() {
        let mut box_ = mailbox();
        assert!(matches!(
            box_.delete_label("INBOX"),
            Err(SimError::SystemLabelIsImmutable(_))
        ));
        assert!(matches!(
            box_.delete_label("Label_99"),
            Err(SimError::NoSuchLabel(_))
        ));
    }

    #[test]
    fn label_counts_are_per_label_and_thread_aware() {
        let mut box_ = mailbox();
        let root = insert(&mut box_, "first");
        box_.insert_message(&MessageSpec::new().reply_to(&root), NOW)
            .unwrap();
        let counts = box_.label_counts("INBOX");
        assert_eq!(counts.messages_total, 2);
        assert_eq!(counts.messages_unread, 2);
        assert_eq!(counts.threads_total, 1, "both messages are one thread");
        assert_eq!(counts.threads_unread, 1);

        box_.mark_read(&root).unwrap();
        assert_eq!(box_.label_counts("INBOX").messages_unread, 1);
        assert_eq!(box_.label_counts("INBOX").threads_unread, 1);
    }

    #[test]
    fn the_search_index_reads_decoded_text_from_encoded_headers() {
        let mut box_ = mailbox();
        box_.insert_message(
            &MessageSpec::new()
                .subject("Café ☕ Rechnung")
                .from("Grüße <hallo@beispiel.de>")
                .text("Ihre Bestellung ist unterwegs"),
            NOW,
        )
        .unwrap();
        // The stored bytes are RFC 2047 encoded…
        let raw = String::from_utf8_lossy(&box_.messages_newest_first()[0].raw).into_owned();
        assert!(raw.contains("=?UTF-8?B?"));
        // …and the index still finds the decoded words.
        assert_eq!(
            box_.search(&Query::parse("subject:café"), NOW, false, &[])
                .len(),
            1
        );
        assert_eq!(
            box_.search(&Query::parse("bestellung"), NOW, false, &[])
                .len(),
            1
        );
        assert_eq!(
            box_.search(&Query::parse("subject:tea"), NOW, false, &[])
                .len(),
            0
        );
    }

    #[test]
    fn the_label_filter_parameter_is_an_and_not_an_or() {
        let mut box_ = mailbox();
        let starred = insert(&mut box_, "starred");
        insert(&mut box_, "plain");
        box_.add_label(&starred, "STARRED").unwrap();

        let both = vec!["INBOX".to_owned(), "STARRED".to_owned()];
        assert_eq!(box_.search(&Query::All, NOW, false, &both).len(), 1);
        let inbox = vec!["INBOX".to_owned()];
        assert_eq!(box_.search(&Query::All, NOW, false, &inbox).len(), 2);
    }

    #[test]
    fn date_headers_are_recovered_from_raw_bytes() {
        let raw = b"Date: Sat, 01 Aug 2026 00:00:00 +0000\r\nSubject: x\r\n\r\nbody";
        assert_eq!(date_header_ms(raw), Some(NOW));
        assert_eq!(date_header_ms(b"Subject: x\r\n\r\nbody"), None);
        assert_eq!(date_header_ms(b"Date: nonsense\r\n\r\nbody"), None);
    }

    #[test]
    fn kind_of_reports_the_label_type() {
        assert_eq!(kind_of(&Label::system("INBOX")), LabelType::System);
        assert_eq!(kind_of(&Label::user("Label_1", "x")), LabelType::User);
    }
}
