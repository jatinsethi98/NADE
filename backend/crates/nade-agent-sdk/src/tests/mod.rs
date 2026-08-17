//! The edge-case checklist in `CRITERIA.md`, one test at a time.

mod approval;
mod caps;
mod defects;
mod edges;
mod happy;
mod replay;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::engine::{Engine, EngineConfig};
use crate::ids::{RunId, Seq};
use crate::journal::{Entry, EntryKind};
use crate::testing::{MemoryJournal, ScriptedLlm};
use crate::tool::Tool;

/// Small caps so a test can reach a boundary in a few turns.
fn config() -> EngineConfig {
    EngineConfig {
        max_steps: 5,
        token_budget: 10_000,
        max_tool_result_bytes: 4096,
        approval_ttl: Some(Duration::from_secs(3600)),
        clock_skew_leeway: Duration::from_secs(60),
        ..EngineConfig::default()
    }
}

/// An engine over a scripted model, an in-memory journal and the given tools.
fn engine(
    responses: Vec<crate::message::ChatResponse>,
    tools: Vec<Arc<dyn Tool>>,
    config: EngineConfig,
) -> (
    Engine<Arc<ScriptedLlm>, MemoryJournal>,
    Arc<ScriptedLlm>,
    MemoryJournal,
) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let journal = MemoryJournal::new();
    let engine = Engine::new(Arc::clone(&llm), tools, journal.clone(), config)
        .expect("engine builds from a valid tool set");
    (engine, llm, journal)
}

/// Pin the exact `(seq, kind)` shape of a run's journal.
fn assert_layout(journal: &MemoryJournal, run: RunId, expected: &[(Seq, &str)]) {
    let recorded = journal.layout(run);
    let actual: Vec<(Seq, &str)> = recorded.iter().map(|(s, k)| (*s, k.as_str())).collect();
    assert_eq!(actual, expected, "journal layout");
}

/// Every entry of `kind`, in order.
fn entries_of(journal: &MemoryJournal, run: RunId, kind: EntryKind) -> Vec<Entry> {
    journal
        .entries(run)
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

/// The payload of the only entry of `kind`, panicking if there is not exactly one.
fn only_payload<P: serde::de::DeserializeOwned>(
    journal: &MemoryJournal,
    run: RunId,
    kind: EntryKind,
) -> P {
    let found = entries_of(journal, run, kind.clone());
    assert_eq!(
        found.len(),
        1,
        "expected exactly one '{kind}' entry, found {}",
        found.len()
    );
    found[0].payload_as(run).expect("payload decodes")
}

/// Dig a nested field out of a JSON value by path.
fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = value;
    for key in path {
        current = current.get(key).unwrap_or_else(|| {
            panic!("no field '{key}' in {current}");
        });
    }
    current
}
