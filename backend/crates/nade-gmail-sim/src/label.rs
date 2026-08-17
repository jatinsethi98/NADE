//! Labels, including Gmail's awkward parts.
//!
//! Two things a nicer-than-reality simulator would smooth over, and must not:
//!
//! 1. **A system label's `name` is its `id`.** `INBOX` is named `INBOX`, not
//!    "Inbox". A client that renders `label.name` shows the user `CATEGORY_
//!    PROMOTIONS` unless it special-cases system labels, and it should find that
//!    out here.
//! 2. **`labels.list` does not return the counters.** `messagesTotal`,
//!    `messagesUnread`, `threadsTotal` and `threadsUnread` are populated by
//!    `labels.get` only. A client that lists labels and reads `messagesUnread`
//!    gets `None` from the real API, so it must get `None` here too.
//!
//! Nor does `labels.list` return the visibility fields for *system* labels — it
//! returns them for user labels only. That asymmetry is real and is reproduced.

use serde_json::{json, Value};

/// `Label.type` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelType {
    /// Built into Gmail. `name == id`, cannot be created or deleted.
    System,
    /// Created by the user. Ids look like `Label_7`.
    User,
}

impl LabelType {
    /// The wire value: `"system"` or `"user"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

/// A label in the mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// `INBOX`, `Label_3`, …
    pub id: String,
    /// For system labels this is byte-identical to [`Self::id`].
    pub name: String,
    /// System or user.
    pub kind: LabelType,
    /// `show` | `hide`. Gmail omits this for system labels in `labels.list`.
    pub message_list_visibility: Visibility,
    /// `labelShow` | `labelShowIfUnread` | `labelHide`.
    pub label_list_visibility: LabelListVisibility,
}

/// `Label.messageListVisibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Threads with this label appear in the message list.
    Show,
    /// They do not.
    Hide,
}

impl Visibility {
    /// The wire value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Hide => "hide",
        }
    }
}

/// `Label.labelListVisibility`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelListVisibility {
    /// Always shown in the label list.
    Show,
    /// Shown only while the label has unread mail.
    ShowIfUnread,
    /// Hidden.
    Hide,
}

impl LabelListVisibility {
    /// The wire value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Show => "labelShow",
            Self::ShowIfUnread => "labelShowIfUnread",
            Self::Hide => "labelHide",
        }
    }
}

/// Counters that only `labels.get` returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LabelCounts {
    /// Messages carrying the label.
    pub messages_total: u64,
    /// …of which unread.
    pub messages_unread: u64,
    /// Threads with at least one message carrying the label.
    pub threads_total: u64,
    /// …of which have at least one unread message carrying it.
    pub threads_unread: u64,
}

impl Label {
    /// Build a system label. `name` is forced to equal `id`.
    #[must_use]
    pub fn system(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: id.to_owned(),
            kind: LabelType::System,
            message_list_visibility: Visibility::Show,
            label_list_visibility: LabelListVisibility::Show,
        }
    }

    /// Build a user label.
    #[must_use]
    pub fn user(id: &str, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: LabelType::User,
            message_list_visibility: Visibility::Show,
            label_list_visibility: LabelListVisibility::Show,
        }
    }

    /// The `labels.list` projection.
    ///
    /// System labels get three fields; user labels get five. No counters, ever
    /// — that is `labels.get`'s job.
    #[must_use]
    pub fn list_json(&self) -> Value {
        let mut object = json!({
            "id": self.id,
            "name": self.name,
            "type": self.kind.as_str(),
        });
        if self.kind == LabelType::User {
            object["messageListVisibility"] = json!(self.message_list_visibility.as_str());
            object["labelListVisibility"] = json!(self.label_list_visibility.as_str());
        }
        object
    }

    /// The `labels.get` projection: everything, counters included.
    #[must_use]
    pub fn get_json(&self, counts: LabelCounts) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "type": self.kind.as_str(),
            "messageListVisibility": self.message_list_visibility.as_str(),
            "labelListVisibility": self.label_list_visibility.as_str(),
            "messagesTotal": counts.messages_total,
            "messagesUnread": counts.messages_unread,
            "threadsTotal": counts.threads_total,
            "threadsUnread": counts.threads_unread,
        })
    }
}

/// Gmail's system labels, in the order `labels.list` returns them on a fresh
/// account.
///
/// The order is part of the fixture: a client that sorts them itself is fine, a
/// client that indexes `labels[0]` is not, and only a stable order can tell the
/// difference.
pub const SYSTEM_LABELS: [&str; 14] = [
    "CHAT",
    "SENT",
    "INBOX",
    "IMPORTANT",
    "TRASH",
    "DRAFT",
    "SPAM",
    "CATEGORY_FORUMS",
    "CATEGORY_UPDATES",
    "CATEGORY_PERSONAL",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "STARRED",
    "UNREAD",
];

/// The label ids `messages.list` hides unless `includeSpamTrash=true`.
pub const HIDDEN_WITHOUT_SPAM_TRASH: [&str; 2] = ["SPAM", "TRASH"];

/// Is this id one of Gmail's built-ins?
#[must_use]
pub fn is_system(id: &str) -> bool {
    SYSTEM_LABELS.contains(&id)
}

/// Normalise a `label:` query term the way Gmail does: case-insensitive, and
/// `-` stands in for a space so `label:my-label` finds "my label".
#[must_use]
pub fn normalise_label_query(term: &str) -> String {
    term.trim()
        .trim_matches('"')
        .to_ascii_lowercase()
        .replace('-', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_labels_are_named_after_themselves() {
        for id in SYSTEM_LABELS {
            let label = Label::system(id);
            assert_eq!(label.name, label.id, "system label {id} must be self-named");
        }
    }

    #[test]
    fn labels_list_omits_the_counters_and_the_system_visibilities() {
        let inbox = Label::system("INBOX").list_json();
        let object = inbox.as_object().unwrap();
        assert_eq!(object.len(), 3, "system label in list: {inbox}");
        assert!(!object.contains_key("messagesTotal"));
        assert!(!object.contains_key("messageListVisibility"));

        let mine = Label::user("Label_1", "Receipts").list_json();
        let object = mine.as_object().unwrap();
        assert_eq!(object.len(), 5, "user label in list: {mine}");
        assert_eq!(object["messageListVisibility"], "show");
        assert_eq!(object["labelListVisibility"], "labelShow");
        assert!(!object.contains_key("messagesTotal"));
    }

    #[test]
    fn labels_get_carries_the_counters() {
        let counts = LabelCounts {
            messages_total: 9,
            messages_unread: 2,
            threads_total: 7,
            threads_unread: 1,
        };
        let body = Label::system("INBOX").get_json(counts);
        assert_eq!(body["messagesTotal"], 9);
        assert_eq!(body["messagesUnread"], 2);
        assert_eq!(body["threadsTotal"], 7);
        assert_eq!(body["threadsUnread"], 1);
        // Counters are numbers, not strings — unlike historyId.
        assert!(body["messagesTotal"].is_number());
    }

    #[test]
    fn label_query_terms_fold_case_and_hyphens() {
        assert_eq!(normalise_label_query("INBOX"), "inbox");
        assert_eq!(normalise_label_query("my-label"), "my label");
        assert_eq!(normalise_label_query("\"My Label\""), "my label");
    }
}
