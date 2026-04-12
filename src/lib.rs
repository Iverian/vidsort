#![warn(clippy::pedantic)]
// Documentation lints are suppressed: this is an internal library crate, not a
// published API, so `# Errors` / `# Panics` sections and `#[must_use]` on every
// public item would add noise without value.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::cast_possible_truncation
)]

pub mod classifier;
pub mod config;
pub mod fetcher;
pub mod linker;
pub mod listener;
pub mod metrics;
pub mod parser;
pub mod pipeline;
pub mod report;
pub mod server;
pub mod transmission;
pub mod tvdb;
pub mod types;
