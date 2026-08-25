//! The deterministic prefilter, and the defaults that keep an old spec alive.

use serde_json::json;

use super::*;

fn labels(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

struct Message {
    from_email: String,
    from_name: String,
    subject: String,
    body: String,
    labels: Vec<String>,
    has_attachments: bool,
    age_days: Option<i64>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            from_email: "priya@kettle.com".to_owned(),
            from_name: "Priya Raghavan".to_owned(),
            subject: "Staff Product Designer at Kettle".to_owned(),
            body: "Hi Jatin — I came across your portfolio.".to_owned(),
            labels: labels(&["INBOX", "CATEGORY_PERSONAL"]),
            has_attachments: false,
            age_days: Some(0),
        }
    }
}

impl Message {
    fn view(&self) -> Candidate<'_> {
        Candidate {
            from_email: &self.from_email,
            from_name: &self.from_name,
            subject: &self.subject,
            body_text: &self.body,
            label_ids: &self.labels,
            has_attachments: self.has_attachments,
            age_days: self.age_days,
        }
    }
}

// ------------------------------------------------------------ the parser --

#[test]
fn a_spec_the_compiler_writes_today_round_trips() {
    let spec = Spec::parse(Some(&json!({
        "version": 1,
        "trigger": {
            "kind": "mail",
            "filters": {
                "from_domains": ["kettle.com"],
                "from_contains": [],
                "subject_contains": ["designer"],
                "body_contains": [],
                "label_ids": ["INBOX"],
                "has_attachment": null,
                "newer_than_days": 30
            },
            "semantic": "The sender is a recruiter."
        },
        "instruction": "Read the thread.",
        "tools": ["read_thread", "write_note"],
        "output": {"kind": "note", "title_template": null}
    })))
    .expect("a compiled spec parses");

    assert!(spec.is_mail_triggered());
    assert_eq!(spec.trigger.filters.from_domains, vec!["kettle.com"]);
    assert_eq!(
        spec.trigger.semantic.as_deref(),
        Some("The sender is a recruiter.")
    );
    assert_eq!(spec.tools, vec!["read_thread", "write_note"]);
}

/// EDGE (crash mid-step, in the shape it actually takes here): a spec written
/// by an older build is missing whatever a newer field is called. D66 is the
/// record of a decode failure taking a whole list down with it; here it would
/// take an agent offline, which is worse than defaulting.
#[test]
fn a_spec_missing_everything_defaults_to_manual_and_matches_nothing_new() {
    let spec = Spec::parse(Some(&json!({}))).expect("an object always parses");
    assert!(!spec.is_mail_triggered(), "the safe default is manual");
    assert_eq!(spec.trigger.kind, TriggerKind::Manual);
    assert!(spec.trigger.semantic.is_none());
    // An empty filter set is not a constraint, which is only safe *because* the
    // trigger defaulted to manual.
    assert!(spec.trigger.filters.matches(&Message::default().view()));
}

#[test]
fn an_unknown_trigger_kind_is_manual_not_an_error() {
    let spec = Spec::parse(Some(&json!({"trigger": {"kind": "telepathy"}}))).expect("parses");
    assert_eq!(spec.trigger.kind, TriggerKind::Manual);
}

#[test]
fn a_null_or_scalar_spec_is_none() {
    assert!(Spec::parse(None).is_none());
    assert!(Spec::parse(Some(&json!(null))).is_none());
    assert!(Spec::parse(Some(&json!("mail"))).is_none());
    assert!(Spec::parse(Some(&json!([1, 2, 3]))).is_none());
}

// ------------------------------------------------------------ the filters --

#[test]
fn no_filters_match_everything() {
    assert!(Filters::default().matches(&Message::default().view()));
}

#[test]
fn from_domains_is_any_and_covers_subdomains_but_not_lookalikes() {
    let filters = Filters {
        from_domains: vec!["kettle.com".to_owned(), "halcyon.io".to_owned()],
        ..Filters::default()
    };
    let mut message = Message::default();
    assert!(filters.matches(&message.view()), "the first domain");

    message.from_email = "jobs@HALCYON.io".to_owned();
    assert!(
        filters.matches(&message.view()),
        "any, and case-insensitive"
    );

    message.from_email = "jobs@careers.kettle.com".to_owned();
    assert!(
        filters.matches(&message.view()),
        "a sub-domain is the company"
    );

    message.from_email = "spam@notkettle.com".to_owned();
    assert!(
        !filters.matches(&message.view()),
        "a lookalike suffix is not a sub-domain"
    );

    message.from_email = "kettle.com@evil.example".to_owned();
    assert!(
        !filters.matches(&message.view()),
        "the domain is what follows the last @, not any substring"
    );
}

#[test]
fn a_filter_domain_may_be_written_with_its_at_sign() {
    let filters = Filters {
        from_domains: vec!["@kettle.com".to_owned(), "  ".to_owned()],
        ..Filters::default()
    };
    assert!(filters.matches(&Message::default().view()));
}

#[test]
fn label_ids_is_any() {
    let filters = Filters {
        label_ids: labels(&["Label_25", "STARRED"]),
        ..Filters::default()
    };
    let mut message = Message::default();
    assert!(!filters.matches(&message.view()));
    message.labels.push("STARRED".to_owned());
    assert!(filters.matches(&message.view()), "any of them, not all");
}

#[test]
fn the_contains_filters_are_all_not_any() {
    let filters = Filters {
        subject_contains: vec!["Staff".to_owned(), "Kettle".to_owned()],
        ..Filters::default()
    };
    let mut message = Message::default();
    assert!(filters.matches(&message.view()), "both substrings present");

    message.subject = "Staff Engineer at Halcyon".to_owned();
    assert!(!filters.matches(&message.view()), "one is not enough");
}

#[test]
fn from_contains_sees_the_display_name_as_well_as_the_address() {
    let filters = Filters {
        from_contains: vec!["raghavan".to_owned()],
        ..Filters::default()
    };
    assert!(
        filters.matches(&Message::default().view()),
        "a display name is part of the From"
    );
}

/// EDGE (empty input): a filter that says nothing must not read as a filter
/// that says everything. `"".contains("")` is true, so an empty needle would
/// silently match every message.
#[test]
fn an_empty_or_blank_needle_is_dropped_rather_than_matched() {
    let filters = Filters {
        subject_contains: vec![String::new(), "   ".to_owned()],
        ..Filters::default()
    };
    let message = Message {
        subject: String::new(),
        ..Message::default()
    };
    assert!(filters.matches(&message.view()));
}

/// EDGE (unicode): case folding is full Unicode on both sides. D64 is the
/// record of why a *length* may never be carried across it; a bool may.
#[test]
fn matching_is_unicode_case_insensitive() {
    let filters = Filters {
        subject_contains: vec!["FÖHN".to_owned()],
        body_contains: vec!["日本語".to_owned()],
        ..Filters::default()
    };
    let message = Message {
        subject: "Ihre Sendung von föhn versand".to_owned(),
        body: "こんにちは 日本語 です".to_owned(),
        ..Message::default()
    };
    assert!(filters.matches(&message.view()));
}

#[test]
fn has_attachment_matches_both_ways_and_null_does_not_constrain() {
    let mut message = Message::default();
    let wants = Filters {
        has_attachment: Some(true),
        ..Filters::default()
    };
    let wants_not = Filters {
        has_attachment: Some(false),
        ..Filters::default()
    };
    assert!(!wants.matches(&message.view()));
    assert!(wants_not.matches(&message.view()));

    message.has_attachments = true;
    assert!(wants.matches(&message.view()));
    assert!(!wants_not.matches(&message.view()));
    assert!(Filters::default().matches(&message.view()), "null: no rule");
}

/// EDGE (clock skew): a message with no date fails a freshness filter rather
/// than passing it, and one stamped in the future is not old.
#[test]
fn newer_than_days_fails_closed_on_an_undated_message() {
    let filters = Filters {
        newer_than_days: Some(30),
        ..Filters::default()
    };
    let aged = |age_days| Message {
        age_days,
        ..Message::default()
    };
    assert!(
        filters.matches(&aged(Some(30)).view()),
        "the boundary is inclusive"
    );
    assert!(!filters.matches(&aged(Some(31)).view()));
    assert!(
        !filters.matches(&aged(None).view()),
        "an undated message must not fire an agent"
    );
}

#[test]
fn every_filter_is_anded_with_every_other() {
    let filters = Filters {
        from_domains: vec!["kettle.com".to_owned()],
        subject_contains: vec!["designer".to_owned()],
        label_ids: labels(&["INBOX"]),
        newer_than_days: Some(30),
        ..Filters::default()
    };
    let mut message = Message::default();
    assert!(filters.matches(&message.view()));

    // Each one, broken alone.
    let broken = Message {
        from_email: "someone@halcyon.io".to_owned(),
        ..Message::default()
    };
    assert!(!filters.matches(&broken.view()));

    let broken = Message {
        subject: "Coffee?".to_owned(),
        ..Message::default()
    };
    assert!(!filters.matches(&broken.view()));

    let broken = Message {
        labels: labels(&["SENT"]),
        ..Message::default()
    };
    assert!(!filters.matches(&broken.view()));

    message.age_days = Some(400);
    assert!(!filters.matches(&message.view()));
}
