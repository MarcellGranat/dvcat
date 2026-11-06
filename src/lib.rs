//! dvc_tui: a terminal UI for DVC projects. The binary in `main.rs` is a thin shell around this.

pub mod app;
pub mod cli;
pub mod experiments;
pub mod graph;
pub mod log;
pub mod project;
pub mod runner;
pub mod stale;
pub mod ui;

#[cfg(test)]
mod test_support;
