//! The compiled `spec`, read back as a type.
//!
//! `agents.spec` is `jsonb` written by [`compile`](super::compile) and was read
//! only in fragments — `spec["instruction"]`, `spec["trigger"]["kind"]` twice,
//! `spec["tools"]` — four ad-hoc probes in two modules. The mail trigger has to
//! read the *whole* thing on every new message, for every published agent, so
//! it gets a type.
//!
//! **And the type is the only reader.** Introducing it and leaving the probes
//! in place would have been worse than either: the trigger vocabulary would
//! have had two authorities that could disagree about an unknown `kind`, and
//! `Spec::instruction` defaults to `""` where the run path falls back to
//! `nl_definition` — so a "safe" substitution would have handed a model an
//! empty task. [`Spec::instruction_or`] is where that fallback now lives.
//! `api::agents` and `agents::run` go through here; the one raw read left is
//! the SQL narrowing hint in `agents::triage`, which is re-checked in Rust and
//! documented as such.
//!
//! # Every field has a default, deliberately
//!
//! A stored spec was written by whatever build compiled it. A field added later
//! is simply absent from every agent already in the table, and a decode that
//! failed on absence would take the agent offline rather than the field. That
//! is D66's lesson pointed the other way: there, an unvalidated `PATCH` could
//! write a `schedule` that failed the app's decode of the *whole* agent list;
//! here, one unreadable spec must not stop the other agents being triaged.
//!
//! Consequently [`Spec::parse`] never fails on shape. It fails only on "this is
//! not an object at all", which is the one case where defaulting would invent
//! an agent that matches everything.

use serde::Deserialize;
use serde_json::Value;

/// `spec.trigger.kind` (`API.md` §5.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// New mail fires it.
    Mail,
    /// A recurrence fires it (P7).
    Schedule,
    /// Only a person fires it. The default, because it is the one that costs
    /// nothing: a spec whose `kind` we could not read must not start
    /// subscribing to the mailbox.
    #[default]
    #[serde(other)]
    Manual,
}

/// The deterministic prefilter. All optional, and **all ANDed** with each other.
///
/// Within one list the rule differs by field, and `API.md` §5.1 now says so:
///
/// * `from_domains` and `label_ids` are **ANY** — a sender has exactly one
///   domain, so ALL would be unsatisfiable, and "restrict to these mailboxes"
///   means any of them;
/// * `from_contains`, `subject_contains` and `body_contains` are **ALL**, which
///   is what the compiler's own prompt tells the model
///   ("Substrings the subject *must contain*").
///
/// In v1 only the compiler writes a spec — `PATCH /agents/{id}` accepts no
/// `spec` — and it never emits `from_contains`, `body_contains` or
/// `has_attachment`. They are implemented and tested because the column is
/// `jsonb` and the contract names them, not because anything reaches them yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub from_domains: Vec<String>,
    pub from_contains: Vec<String>,
    pub subject_contains: Vec<String>,
    pub body_contains: Vec<String>,
    pub label_ids: Vec<String>,
    pub has_attachment: Option<bool>,
    pub newer_than_days: Option<i64>,
}

/// `spec.trigger`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub filters: Filters,
    /// The judgement a filter cannot express, checked by the cheap model on the
    /// rows the filters kept. `None` means the filters are the whole answer.
    pub semantic: Option<String>,
}

/// `spec.output`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Output {
    pub kind: Option<String>,
    pub title_template: Option<String>,
}

/// A compiled agent definition (`API.md` §5.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub version: u32,
    pub trigger: Trigger,
    pub instruction: String,
    pub tools: Vec<String>,
    pub output: Output,
}

impl Spec {
    /// Read a stored spec.
    ///
    /// `None` for a null column, and for a value that is not a JSON object —
    /// the one shape where defaulting would invent a trigger the user never
    /// wrote. Anything else decodes, with defaults for what is missing.
    #[must_use]
    pub fn parse(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        if !value.is_object() {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }

    /// Does new mail fire this agent at all?
    #[must_use]
    pub fn is_mail_triggered(&self) -> bool {
        self.trigger.kind == TriggerKind::Mail
    }

    /// Is this agent on a recurrence? The guard `PATCH /agents/{id}` uses
    /// before it will accept a `schedule`.
    #[must_use]
    pub fn is_scheduled(&self) -> bool {
        self.trigger.kind == TriggerKind::Schedule
    }

    /// The run's task sentence, or the user's own words when the spec has none.
    ///
    /// The fallback is the caller's rather than this type's, and it matters:
    /// `Spec::instruction` defaults to `""`, but the *system prompt* of every
    /// run falls back to `agents.nl_definition` — an empty task sentence would
    /// leave a model with no instruction at all. Stating it here is what stops
    /// the next reader of `spec["instruction"]` getting it wrong.
    #[must_use]
    pub fn instruction_or<'a>(&'a self, nl_definition: &'a str) -> &'a str {
        if self.instruction.trim().is_empty() {
            nl_definition
        } else {
            &self.instruction
        }
    }

    /// The tool names the spec asks for (`API.md` §5.1).
    #[must_use]
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// The trigger's kind, for a caller that only needs to branch on it.
    #[must_use]
    pub fn trigger_kind(&self) -> TriggerKind {
        self.trigger.kind
    }
}

/// The facts about one message the filters are evaluated against.
///
/// A borrowed view, built once per message and shared across every agent, so a
/// mailbox with five agents costs one row read rather than five.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub from_email: &'a str,
    pub from_name: &'a str,
    pub subject: &'a str,
    pub body_text: &'a str,
    pub label_ids: &'a [String],
    pub has_attachments: bool,
    /// Whole days since the message's own `internal_ts`. `None` when the
    /// message carries no date.
    pub age_days: Option<i64>,
}

impl Filters {
    /// Does this message survive the prefilter?
    #[must_use]
    pub fn matches(&self, message: &Candidate<'_>) -> bool {
        // Lowered once each, not once per needle. `to_lowercase` is full
        // Unicode case mapping and is *not* length-preserving (D64), which is
        // safe here only because nothing indexes back into the original: the
        // answer is a bool, never a span.
        //
        // **And lowered only when a needle asks for it.** This runs once per
        // published agent per inbound message, and the compiler leaves
        // `body_contains` empty for every agent it writes — so lowering the
        // whole body up front bought a full case mapping of somebody's email,
        // per agent, to answer a constraint nobody set.
        let from_email = message.from_email.to_lowercase();

        if !self.from_domains.is_empty() {
            let domain = from_email.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
            let any = self.from_domains.iter().any(|filter| {
                let filter = filter.trim().trim_start_matches('@').to_lowercase();
                // A sub-domain of the filter counts: mail from
                // `careers.kettle.com` is mail from Kettle. The `.` is what
                // stops `kettle.com` also matching `notkettle.com`.
                !filter.is_empty() && (domain == filter || domain.ends_with(&format!(".{filter}")))
            });
            if !any {
                return false;
            }
        }

        if !self.label_ids.is_empty() {
            let any = self
                .label_ids
                .iter()
                .any(|want| message.label_ids.iter().any(|have| have == want));
            if !any {
                return false;
            }
        }

        let unmatched = |needles: &[String], haystack: &dyn Fn() -> String| {
            !needles.is_empty() && !contains_all(&haystack(), needles)
        };
        if unmatched(&self.from_contains, &|| {
            format!("{} {}", message.from_name, message.from_email).to_lowercase()
        }) || unmatched(&self.subject_contains, &|| message.subject.to_lowercase())
            || unmatched(&self.body_contains, &|| message.body_text.to_lowercase())
        {
            return false;
        }

        if let Some(want) = self.has_attachment {
            if message.has_attachments != want {
                return false;
            }
        }

        if let Some(days) = self.newer_than_days {
            // EDGE (clock skew, and a message with no date): an undated message
            // fails a freshness filter rather than passing it. Failing closed
            // costs a run that would probably have been wrong; failing open
            // would fire an agent on mail of unknown age.
            match message.age_days {
                Some(age) if age <= days => {}
                _ => return false,
            }
        }

        true
    }
}

/// Every needle present, on an already-lowered haystack.
///
/// EDGE (empty input): no needles is not a constraint, so it passes. A needle
/// that is empty or whitespace-only is dropped rather than matched — an empty
/// substring is in every string, which would make a filter that says nothing
/// look like a filter that says everything.
fn contains_all(haystack: &str, needles: &[String]) -> bool {
    needles
        .iter()
        .map(|needle| needle.trim().to_lowercase())
        .filter(|needle| !needle.is_empty())
        .all(|needle| haystack.contains(&needle))
}

#[cfg(test)]
mod tests;
