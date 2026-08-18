//! Mail parsing: raw RFC-822 bytes in, the fields `API.md` §2 promises out.
//!
//! `docs/PARSER.md` is the specification. It records a recipe already validated
//! by a throwaway probe against all 26 conformance cases and 60 real messages,
//! plus the three traps that probe found. This module implements that recipe; it
//! does not re-explore it.

pub mod cache;
pub mod html;
pub mod parse;
