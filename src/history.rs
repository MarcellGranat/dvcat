//! Run times: how they are written, and the small file that keeps them between sessions.
//!
//! DVC records no durations, so the interface remembers the runs it started itself. The file lives
//! in `.dvc/tmp/`, which DVC already keeps out of Git; a project without `.dvc` keeps nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::runner::RunOutcome;

const FILE: &str = ".dvc/tmp/dvcat-history.json";

/// `850ms`, `4.2s`, `2m 05s`, `1h 02m`.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else if seconds >= 1 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// How a remembered stage run ended (a run that never started is not remembered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredOutcome {
    Succeeded,
    Failed { code: Option<i32> },
    Cancelled,
}

impl StoredOutcome {
    pub fn from_outcome(outcome: &RunOutcome) -> Option<Self> {
        match outcome {
            RunOutcome::Succeeded => Some(Self::Succeeded),
            RunOutcome::Failed { code } => Some(Self::Failed { code: *code }),
            RunOutcome::Cancelled => Some(Self::Cancelled),
            RunOutcome::NotStarted(_) => None,
        }
    }

    pub fn to_outcome(self) -> RunOutcome {
        match self {
            Self::Succeeded => RunOutcome::Succeeded,
            Self::Failed { code } => RunOutcome::Failed { code },
            Self::Cancelled => RunOutcome::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoredRun {
    pub outcome: StoredOutcome,
    pub seconds: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// Latest finished run of each stage.
    #[serde(default)]
    pub stages: BTreeMap<String, StoredRun>,
    /// Seconds each experiment took, by experiment name.
    #[serde(default)]
    pub experiments: BTreeMap<String, f64>,
    /// Param columns hidden in the experiments table.
    #[serde(default)]
    pub hidden_params: BTreeSet<String>,
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Read the history; a missing or unreadable file is an empty history, never an error.
pub fn load(root: &Path) -> History {
    fs::read_to_string(path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the history. Does nothing for a project without a `.dvc` directory.
pub fn save(root: &Path, history: &History) -> io::Result<()> {
    if !root.join(".dvc").is_dir() {
        return Ok(());
    }
    let target = path(root);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(history).map_err(io::Error::other)?;
    fs::write(target, text)
}

/// Names from DVC's `Ran experiment(s): a, b` line.
pub fn experiments_named_in(log_lines: impl Iterator<Item = String>) -> Vec<String> {
    const PREFIX: &str = "Ran experiment(s): ";
    log_lines
        .filter_map(|line| line.trim().strip_prefix(PREFIX).map(str::to_owned))
        .flat_map(|names| {
            names
                .split(',')
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally_at_every_scale() {
        let cases = [
            (Duration::from_millis(0), "0ms"),
            (Duration::from_millis(850), "850ms"),
            (Duration::from_millis(4_200), "4.2s"),
            (Duration::from_secs(59), "59.0s"),
            (Duration::from_secs(125), "2m 05s"),
            (Duration::from_secs(3_720), "1h 02m"),
        ];
        for (duration, expected) in cases {
            assert_eq!(format_duration(duration), expected);
        }
    }

    #[test]
    fn history_survives_a_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".dvc")).unwrap();
        let mut history = History::default();
        history.stages.insert(
            "train".into(),
            StoredRun {
                outcome: StoredOutcome::Failed { code: Some(2) },
                seconds: 4.25,
            },
        );
        history.experiments.insert("lr-005".into(), 12.5);
        history.hidden_params.insert("params.yaml:seed".into());
        save(dir.path(), &history).unwrap();
        assert_eq!(load(dir.path()), history);
    }

    #[test]
    fn a_project_without_dvc_keeps_nothing_and_a_bad_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &History::default()).unwrap();
        assert!(
            !dir.path().join(".dvc").exists(),
            "no directory is invented"
        );
        fs::create_dir_all(dir.path().join(".dvc/tmp")).unwrap();
        fs::write(path(dir.path()), "{ not json").unwrap();
        assert_eq!(load(dir.path()), History::default());
        assert_eq!(load(&dir.path().join("missing")), History::default());
    }

    #[test]
    fn unknown_or_missing_fields_do_not_break_loading() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".dvc/tmp")).unwrap();
        fs::write(
            path(dir.path()),
            r#"{"experiments": {"a": 1.5}, "future": true}"#,
        )
        .unwrap();
        let history = load(dir.path());
        assert_eq!(history.experiments["a"], 1.5);
        assert!(history.stages.is_empty());
    }

    #[test]
    fn outcomes_round_trip_except_runs_that_never_started() {
        for outcome in [
            RunOutcome::Succeeded,
            RunOutcome::Failed { code: Some(3) },
            RunOutcome::Failed { code: None },
            RunOutcome::Cancelled,
        ] {
            let stored = StoredOutcome::from_outcome(&outcome).unwrap();
            assert_eq!(stored.to_outcome(), outcome);
        }
        assert_eq!(
            StoredOutcome::from_outcome(&RunOutcome::NotStarted("x".into())),
            None
        );
    }

    #[test]
    fn experiment_names_are_read_from_the_summary_line() {
        let log = [
            "Reproducing experiment 'x'",
            "Ran experiment(s): lr-1, lr-2",
            "done",
        ];
        let names = experiments_named_in(log.iter().map(|l| (*l).to_owned()));
        assert_eq!(names, ["lr-1", "lr-2"]);
        assert!(experiments_named_in(["nothing"].iter().map(|l| (*l).to_owned())).is_empty());
    }
}
