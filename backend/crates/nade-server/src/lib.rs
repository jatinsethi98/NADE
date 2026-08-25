//! NADE backend.
//!
//! P1 is the foundation: schema, a durable job queue, pairing auth, and one
//! health endpoint. Gmail sync (P2/P3), the agent runtime (P4/P5), ask and push
//! (P6) and schedules (P7) mount onto the seams left here - `api::router`, the
//! `jobs::Registry`, and `api::auth::Auth` in the request extensions.

#![forbid(unsafe_code)]

pub mod agents;
pub mod api;
pub mod config;
pub mod db;
pub mod embedded;
pub mod error;
pub mod gmail;
pub mod jobs;
pub(crate) mod json;
pub mod llm;
pub mod mail;
pub mod runtime;
pub mod search;
pub mod state;
pub mod sync;

#[cfg(test)]
pub(crate) mod test_support;

/// Reported by `GET /v1/healthz`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Every job kind this server can run.
///
/// Lives here rather than in `main.rs` so a test can assert the registry a real
/// process would build. P6 adds nothing; P7 adds the schedule waker.
pub fn register_handlers(registry: &mut jobs::Registry, state: &state::AppState) {
    registry.register(sync::KIND, sync::SyncHandler::shared(state.clone()));
    registry.register(
        sync::incremental::KIND,
        sync::incremental::IncrementalHandler::shared(state.clone()),
    );
    registry.register(
        sync::schedule::KIND,
        sync::schedule::MaintenanceHandler::shared(state.clone()),
    );
    registry.register(
        agents::run::KIND,
        agents::run::RunAgentHandler::shared(state.clone()),
    );
    registry.register(
        agents::resume::KIND,
        agents::resume::ResumeRunHandler::shared(state.clone()),
    );
    registry.register(
        agents::triage::KIND,
        agents::triage::TriageHandler::shared(state.clone()),
    );
    registry.register(
        agents::expire::KIND,
        agents::expire::ExpireApprovalsHandler::shared(state.clone()),
    );
}
