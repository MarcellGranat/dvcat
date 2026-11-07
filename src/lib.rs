//! dvcat: a terminal UI for DVC projects. The binary in `main.rs` is a thin shell around this.

pub mod app;
pub mod cli;
pub mod dag;
pub mod expand;
pub mod experiments;
pub mod external;
pub mod files;
pub mod form;
pub mod graph;
pub mod history;
pub mod log;
pub mod params_filter;
pub mod plot;
pub mod project;
pub mod prompt;
pub mod runner;
pub mod stale;
pub mod theme;
pub mod ui;

#[cfg(test)]
mod test_support;
