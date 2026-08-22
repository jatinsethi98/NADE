//! The model provider: the Anthropic adapter, what a call costs, and the
//! per-account ledger the spend ceiling reads.
//!
//! # Where the boundary is
//!
//! `nade-agent-sdk` owns the engine and defines [`Llm`](nade_agent_sdk::Llm) as
//! "translate a provider-neutral request into a vendor's wire format and back".
//! Everything vendor-shaped therefore lives here, in the host, and the SDK
//! stays free of it — including the two things the SDK deliberately refuses to
//! model: retries (an adapter owns them) and the cache-token breakdown (it
//! differs per vendor).
//!
//! # The one rule that is not obvious
//!
//! An `Err` from `Llm::chat` means, by the trait's contract, *"the host should
//! retry this job later"*. So a spend-ceiling breach cannot be reported that
//! way — the retry is the re-spend. The breach is signalled out of band
//! instead, through [`ledger::Breach`], and the job handler turns it into
//! `Engine::cancel`. See `agents::run`.

pub mod anthropic;
pub mod cost;
pub mod ledger;

/// An [`Llm`](nade_agent_sdk::Llm) that is never asked anything.
///
/// `Engine::cancel` loads the journal, replays it and appends `run_ended`. It
/// dispatches no tool and makes no model call — but `Engine::new` still takes
/// its `Llm` by value, so ending a run used to mean assembling the whole run
/// path: a configured provider, an `accounts` read, a `SpendGuard` and four
/// tool constructions, none of which cancel touches.
///
/// The cost of that was not just the waste. `build_engine` fails outright when
/// no provider is configured, so on a server with no API key a `DELETE` could
/// not end a started run through the engine at all, and fell through to writing
/// the row straight to `failed` — a run with a status its journal does not
/// support, which is exactly what `API.md` §6.1 and D57 forbid.
#[derive(Debug, Clone, Copy)]
pub struct Unreachable;

#[async_trait::async_trait]
impl nade_agent_sdk::Llm for Unreachable {
    async fn chat(
        &self,
        _request: nade_agent_sdk::ChatRequest,
    ) -> nade_agent_sdk::Result<nade_agent_sdk::ChatResponse> {
        // Unreachable by construction, and an error rather than a panic in case
        // it ever stops being: a run that somehow reached here should fail, not
        // take the worker down with it.
        Err(nade_agent_sdk::Error::llm(
            "this engine exists only to end a run; it cannot call a model",
        ))
    }
}

/// The model used when `NADE_LLM_MODEL` is unset.
///
/// The **undated alias** on purpose. A dated snapshot pins behaviour, which is
/// worth having — `backend/.env` pins one — but a snapshot can be retired, and
/// a retired id is a `404` that looks exactly like an outage. The alias is the
/// safe default for a build that might sit unused for months; pinning is a
/// deployment choice, made in `.env`, where it is visible.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

/// Anthropic's API root. Only the test suite overrides it.
pub const DEFAULT_API_BASE: &str = "https://api.anthropic.com";

/// The `anthropic-version` header. Not a config knob: a version bump is a code
/// change, because the response shape it selects is one this crate parses.
pub const API_VERSION: &str = "2023-06-01";

/// Why a model call was made. Persisted verbatim into `llm_calls.purpose`,
/// whose check constraint lists exactly these four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A step inside an agent run.
    Run,
    /// Compiling an `nl_definition` into a spec at save time.
    Compile,
    /// Deciding whether one message matches an agent's semantic trigger (P5).
    Triage,
    /// Answering, searching or drafting from the Ask field (P6).
    Ask,
}

impl Purpose {
    /// The string stored in `llm_calls.purpose`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Compile => "compile",
            Self::Triage => "triage",
            Self::Ask => "ask",
        }
    }
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_purpose_matches_the_check_constraint_in_migration_0005() {
        // The migration's `check (purpose in (...))` and this enum are two
        // spellings of one list; a new variant that is not in the DDL fails
        // every insert at runtime, which is the worst place to find out.
        let ddl = include_str!("../../migrations/0005_agents_runtime.sql");
        for purpose in [
            Purpose::Run,
            Purpose::Compile,
            Purpose::Triage,
            Purpose::Ask,
        ] {
            let quoted = format!("'{}'", purpose.as_str());
            assert!(
                ddl.contains(&quoted),
                "llm_calls.purpose check constraint is missing {quoted}"
            );
        }
    }

    #[test]
    fn the_default_model_is_priced() {
        let (_, known) = cost::rates_for(DEFAULT_MODEL);
        assert!(
            known,
            "the default model must not fall back to the unknown rate"
        );
    }
}
