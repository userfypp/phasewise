//! phasewise: run big plans with Claude Code or Codex one phase at a time —
//! verified, checkpointed, and reviewable.

#![forbid(unsafe_code)]
#![warn(clippy::unwrap_used, clippy::expect_used)]

pub mod agent;
pub mod cli;
pub mod display;
pub mod doctor;
pub mod engine;
pub mod git;
pub mod ops;
pub mod plan;
pub mod process;
pub mod prompt;
pub mod settings;
pub mod state;
pub mod style;
pub mod tui;
pub mod verify;
