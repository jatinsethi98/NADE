//! A stateful, deterministic, in-process simulator of the Gmail REST API v1.
//!
//! Placeholder crate root; the public surface is assembled once every module
//! lands.
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]
// `similar_names` fires on pairs like `ct_params`/`cd_params`, where the
// similarity is the point — they are the Content-Type and Content-Disposition
// parameter lists and renaming either makes the code read worse.
#![allow(clippy::module_name_repetitions, clippy::similar_names)]

pub mod api;
pub mod batch;
pub mod clock;
pub mod error;
pub mod fault;
pub mod history;
pub mod ids;
pub mod label;
pub mod mailbox;
pub mod message;
pub mod mime;
pub mod query;
pub mod render;
pub mod simulator;

pub use clock::{Clock, TestClock};
pub use error::{ApiError, SimError};
pub use message::{MessageSpec, StoredMessage};
pub use simulator::Simulator;
