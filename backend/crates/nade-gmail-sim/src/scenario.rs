//! A whole sync story as one readable value.
//!
//! A [`Scenario`] is a list of [`Step`]s — mutations, clock movements, fault
//! arming — applied to a simulator in order. It exists so a test can *say* what
//! happened to the mailbox instead of interleaving twenty statements with the
//! assertions about them, and so the same story can be replayed against a second
//! simulator to prove the two agree byte for byte.
//!
//! ```
//! use std::time::Duration;
//! use nade_gmail_sim::{MessageSpec, Scenario, Simulator, Target};
//!
//! let sim = Simulator::new();
//! let outcome = Scenario::new()
//!     .insert(MessageSpec::new().subject("first"))
//!     .advance(Duration::from_secs(60))
//!     .insert(MessageSpec::new().subject("second"))
//!     .mark_read(Target::nth(0))
//!     .trash(Target::nth(1))
//!     .run(&sim)
//!     .expect("the scenario applies");
//!
//! assert_eq!(outcome.inserted.len(), 2);
//! // Four mutations, four history records, all of them visible to a sync.
//! assert_eq!(outcome.history_ids.len(), 4);
//! ```

use std::time::Duration;

use crate::{
    error::{Result, SimError},
    fault::FaultRule,
    history::{HistoryIdStep, HistoryRetention},
    message::MessageSpec,
    simulator::Simulator,
};

/// How a step names a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A literal message id.
    Id(String),
    /// The `n`th message *this scenario* inserted, counting from zero.
    ///
    /// The point of this variant: a scenario can be written before the ids
    /// exist, which is what lets a whole story be one value.
    Inserted(usize),
}

impl Target {
    /// The `n`th message this scenario inserted.
    #[must_use]
    pub fn nth(index: usize) -> Self {
        Self::Inserted(index)
    }

    fn resolve(&self, inserted: &[String]) -> Result<String> {
        match self {
            Self::Id(id) => Ok(id.clone()),
            Self::Inserted(index) => inserted.get(*index).cloned().ok_or_else(|| {
                SimError::InvalidSpec(format!(
                    "scenario referred to inserted message #{index}, but only {} were inserted",
                    inserted.len()
                ))
            }),
        }
    }
}

impl From<&str> for Target {
    fn from(value: &str) -> Self {
        Self::Id(value.to_owned())
    }
}

impl From<String> for Target {
    fn from(value: String) -> Self {
        Self::Id(value)
    }
}

/// One thing that happens to the world.
#[derive(Debug, Clone)]
pub enum Step {
    /// Move the clock forward.
    Advance(Duration),
    /// Set the clock to an absolute epoch-millisecond instant.
    SetTimeMs(i64),
    /// Insert a message.
    Insert(Box<MessageSpec>),
    /// Permanently delete one.
    Delete(Target),
    /// Move one to Trash.
    Trash(Target),
    /// Take one out of Trash.
    Untrash(Target),
    /// Add a label.
    AddLabel(Target, String),
    /// Remove a label.
    RemoveLabel(Target, String),
    /// Drop `UNREAD`.
    MarkRead(Target),
    /// Add `UNREAD`.
    MarkUnread(Target),
    /// Change many messages as one history record.
    BulkModify {
        /// Which messages.
        targets: Vec<Target>,
        /// Labels to add.
        add: Vec<String>,
        /// Labels to remove.
        remove: Vec<String>,
    },
    /// Create a user label. Its id lands in [`Outcome::labels`].
    CreateLabel(String),
    /// Delete a user label, stripping it from every message.
    DeleteLabel(String),
    /// Arm a fault.
    Inject(FaultRule),
    /// Disarm every fault.
    ClearFaults,
    /// Shrink the history window, so a cursor can expire.
    SetRetention(HistoryRetention),
    /// Force the history floor upward without generating traffic.
    ExpireHistoryBefore(u64),
    /// Make `historyId` move by exactly one per mutation, for readable output.
    SetHistoryStep(HistoryIdStep),
    /// A label for the reader; recorded in [`Outcome::notes`].
    Note(String),
}

/// What running a scenario produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Message ids, in insertion order.
    pub inserted: Vec<String>,
    /// User label ids, in creation order.
    pub labels: Vec<String>,
    /// Every `historyId` a step produced, in order. A step that changed nothing
    /// contributes **nothing** here — which is how a test asserts that a
    /// redundant mutation really was free.
    pub history_ids: Vec<u64>,
    /// `(step index, note)` for every [`Step::Note`].
    pub notes: Vec<(usize, String)>,
}

/// A script of steps.
#[derive(Debug, Clone, Default)]
pub struct Scenario {
    steps: Vec<Step>,
}

impl Scenario {
    /// An empty scenario.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append any step.
    #[must_use]
    pub fn then(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// The steps, for inspection.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Insert a message.
    #[must_use]
    pub fn insert(self, spec: MessageSpec) -> Self {
        self.then(Step::Insert(Box::new(spec)))
    }

    /// Move the clock forward.
    #[must_use]
    pub fn advance(self, by: Duration) -> Self {
        self.then(Step::Advance(by))
    }

    /// Permanently delete a message.
    #[must_use]
    pub fn delete(self, target: impl Into<Target>) -> Self {
        self.then(Step::Delete(target.into()))
    }

    /// Move a message to Trash.
    #[must_use]
    pub fn trash(self, target: impl Into<Target>) -> Self {
        self.then(Step::Trash(target.into()))
    }

    /// Add a label.
    #[must_use]
    pub fn add_label(self, target: impl Into<Target>, label: impl Into<String>) -> Self {
        self.then(Step::AddLabel(target.into(), label.into()))
    }

    /// Remove a label.
    #[must_use]
    pub fn remove_label(self, target: impl Into<Target>, label: impl Into<String>) -> Self {
        self.then(Step::RemoveLabel(target.into(), label.into()))
    }

    /// Drop `UNREAD`.
    #[must_use]
    pub fn mark_read(self, target: impl Into<Target>) -> Self {
        self.then(Step::MarkRead(target.into()))
    }

    /// Arm a fault.
    #[must_use]
    pub fn inject(self, rule: FaultRule) -> Self {
        self.then(Step::Inject(rule))
    }

    /// Record a note.
    #[must_use]
    pub fn note(self, text: impl Into<String>) -> Self {
        self.then(Step::Note(text.into()))
    }

    /// Apply every step to `sim`, in order.
    ///
    /// # Errors
    /// The first step that fails stops the run and returns its error; steps
    /// before it have already been applied, which is deliberate — a scenario is
    /// a script, not a transaction, and half-applying one is exactly what a
    /// crash mid-sync looks like.
    pub fn run(&self, sim: &Simulator) -> Result<Outcome> {
        let mut outcome = Outcome::default();
        let clock = sim.clock();
        for (index, step) in self.steps.iter().enumerate() {
            match step {
                Step::Advance(by) => clock.advance(*by),
                Step::SetTimeMs(at) => clock.set_ms(*at),
                Step::Insert(spec) => {
                    let id = sim.insert_message(spec)?;
                    let history = sim.mailbox(|mailbox| {
                        mailbox.message(&id).map(|message| message.history_id)
                    });
                    outcome.inserted.push(id);
                    if let Some(Some(history)) = Some(history) {
                        outcome.history_ids.push(history);
                    }
                }
                Step::Delete(target) => {
                    let id = target.resolve(&outcome.inserted)?;
                    record(&mut outcome, sim.delete_message(&id)?);
                }
                Step::Trash(target) => {
                    let id = target.resolve(&outcome.inserted)?;
                    record(&mut outcome, sim.trash_message(&id)?);
                }
                Step::Untrash(target) => {
                    let id = target.resolve(&outcome.inserted)?;
                    let moved = sim.mailbox_mut(|mailbox| mailbox.untrash_message(&id))?;
                    record(&mut outcome, moved);
                }
                Step::AddLabel(target, label) => {
                    let id = target.resolve(&outcome.inserted)?;
                    record(&mut outcome, sim.add_label(&id, label)?);
                }
                Step::RemoveLabel(target, label) => {
                    let id = target.resolve(&outcome.inserted)?;
                    record(&mut outcome, sim.remove_label(&id, label)?);
                }
                Step::MarkRead(target) => {
                    let id = target.resolve(&outcome.inserted)?;
                    record(&mut outcome, sim.mark_read(&id)?);
                }
                Step::MarkUnread(target) => {
                    let id = target.resolve(&outcome.inserted)?;
                    let changed = sim.mailbox_mut(|mailbox| mailbox.mark_unread(&id))?;
                    record(&mut outcome, changed);
                }
                Step::BulkModify {
                    targets,
                    add,
                    remove,
                } => {
                    let ids: Vec<String> = targets
                        .iter()
                        .map(|target| target.resolve(&outcome.inserted))
                        .collect::<Result<_>>()?;
                    let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
                    let added: Vec<&str> = add.iter().map(String::as_str).collect();
                    let removed: Vec<&str> = remove.iter().map(String::as_str).collect();
                    let changed = sim
                        .mailbox_mut(|mailbox| mailbox.bulk_modify(&borrowed, &added, &removed))?;
                    record(&mut outcome, changed);
                }
                Step::CreateLabel(name) => {
                    let id = sim.mailbox_mut(|mailbox| mailbox.create_label(name))?;
                    outcome.labels.push(id);
                }
                Step::DeleteLabel(id) => {
                    let changed = sim.mailbox_mut(|mailbox| mailbox.delete_label(id))?;
                    record(&mut outcome, changed);
                }
                Step::Inject(rule) => sim.inject(rule.clone()),
                Step::ClearFaults => sim.clear_faults(),
                Step::SetRetention(retention) => {
                    sim.mailbox_mut(|mailbox| mailbox.history_mut().set_retention(*retention));
                }
                Step::ExpireHistoryBefore(history_id) => {
                    sim.mailbox_mut(|mailbox| mailbox.history_mut().expire_before(*history_id));
                }
                Step::SetHistoryStep(step) => {
                    sim.mailbox_mut(|mailbox| mailbox.history_mut().set_step(*step));
                }
                Step::Note(text) => outcome.notes.push((index, text.clone())),
            }
        }
        Ok(outcome)
    }
}

fn record(outcome: &mut Outcome, history_id: Option<u64>) {
    if let Some(history_id) = history_id {
        outcome.history_ids.push(history_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fault::Fault, ApiError};

    #[test]
    fn a_scenario_applies_in_order_and_reports_what_it_did() {
        let sim = Simulator::new();
        let outcome = Scenario::new()
            .note("two arrive")
            .insert(MessageSpec::new().subject("one"))
            .advance(Duration::from_secs(30))
            .insert(MessageSpec::new().subject("two"))
            .note("then one is read and one is trashed")
            .mark_read(Target::nth(0))
            .trash(Target::nth(1))
            .run(&sim)
            .unwrap();

        assert_eq!(outcome.inserted.len(), 2);
        assert_eq!(outcome.history_ids.len(), 4);
        assert_eq!(outcome.notes.len(), 2);
        assert_eq!(outcome.notes[0], (0, "two arrive".to_owned()));
        // Strictly increasing: every step wrote history.
        assert!(outcome.history_ids.windows(2).all(|pair| pair[1] > pair[0]));
        assert_eq!(sim.history_id(), *outcome.history_ids.last().unwrap());
    }

    #[test]
    fn a_step_that_changes_nothing_contributes_no_history_id() {
        let sim = Simulator::new();
        let outcome = Scenario::new()
            .insert(MessageSpec::new())
            .mark_read(Target::nth(0))
            .mark_read(Target::nth(0))
            .add_label(Target::nth(0), "INBOX")
            .run(&sim)
            .unwrap();
        assert_eq!(
            outcome.history_ids.len(),
            2,
            "insert + first mark_read only: {:?}",
            outcome.history_ids
        );
    }

    #[test]
    fn the_clock_moves_only_where_the_script_says() {
        let sim = Simulator::new();
        let start = sim.clock().now_ms();
        Scenario::new()
            .insert(MessageSpec::new())
            .insert(MessageSpec::new())
            .run(&sim)
            .unwrap();
        assert_eq!(sim.clock().now_ms(), start, "no step advanced time");

        Scenario::new()
            .advance(Duration::from_secs(90))
            .run(&sim)
            .unwrap();
        assert_eq!(sim.clock().now_ms(), start + 90_000);
    }

    #[test]
    fn an_out_of_range_target_is_a_clear_error() {
        let sim = Simulator::new();
        let outcome = Scenario::new()
            .insert(MessageSpec::new())
            .trash(Target::nth(3))
            .run(&sim);
        let message = outcome.unwrap_err().to_string();
        assert!(message.contains("inserted message #3"), "{message}");
        // The steps before the failure really happened.
        assert_eq!(sim.mailbox(crate::mailbox::Mailbox::message_count), 1);
    }

    #[test]
    fn literal_ids_work_alongside_positional_ones() {
        let sim = Simulator::new();
        let outcome = Scenario::new()
            .insert(MessageSpec::new().id("fixed-id"))
            .add_label("fixed-id", "STARRED")
            .run(&sim)
            .unwrap();
        assert_eq!(outcome.inserted, ["fixed-id"]);
        assert!(sim.mailbox(|mailbox| mailbox.message("fixed-id").unwrap().has_label("STARRED")));
    }

    #[test]
    fn the_same_scenario_against_two_simulators_produces_the_same_world() {
        let script = || {
            Scenario::new()
                .insert(MessageSpec::new().subject("a").text("body a"))
                .advance(Duration::from_secs(3600))
                .insert(MessageSpec::new().subject("b").html_only("<p>b</p>"))
                .add_label(Target::nth(0), "STARRED")
                .trash(Target::nth(1))
        };
        let first = Simulator::new();
        let second = Simulator::new();
        let a = script().run(&first).unwrap();
        let b = script().run(&second).unwrap();
        assert_eq!(a, b);
        assert_eq!(first.history_id(), second.history_id());
        let raws = |sim: &Simulator| {
            sim.mailbox(|mailbox| {
                mailbox
                    .messages_newest_first()
                    .iter()
                    .map(|message| message.raw.clone())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(raws(&first), raws(&second));
    }

    #[test]
    fn faults_and_labels_can_be_scripted_too() {
        let sim = Simulator::new();
        let outcome = Scenario::new()
            .then(Step::CreateLabel("Receipts".to_owned()))
            .insert(MessageSpec::new())
            .then(Step::AddLabel(Target::nth(0), "Label_1".to_owned()))
            .then(Step::DeleteLabel("Label_1".to_owned()))
            .inject(FaultRule::new(Fault::Fail(ApiError::BackendError)).on_nth_call(1))
            .run(&sim)
            .unwrap();
        assert_eq!(outcome.labels, ["Label_1"]);
        assert_eq!(sim.handle(&sim.authorized("/gmail/v1/users/me/profile")).status, 500);
        assert_eq!(sim.handle(&sim.authorized("/gmail/v1/users/me/profile")).status, 200);
    }
}
