//! NADE backend.
//!
//! P1 is the foundation: schema, a durable job queue, pairing auth, and one
//! health endpoint. Gmail sync (P2/P3), the agent runtime (P4/P5), ask and push
//! (P6) and schedules (P7) mount onto the seams left here - `api::router`, the
//! `jobs::Registry`, and `api::auth::Auth` in the request extensions.

#![forbid(unsafe_code)]

pub mod api;
pub mod config;
pub mod db;
pub mod embedded;
pub mod error;
pub mod jobs;
pub mod mail;
pub mod state;

#[cfg(test)]
pub(crate) mod test_support;

/// Reported by `GET /v1/healthz`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
