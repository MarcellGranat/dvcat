//! Experiments: reading `dvc exp show --json` and turning it into a sortable table.
//!
//! Two layers again: private `Raw*` types mirror DVC's JSON (every field that DVC may leave
//! `null` is an `Option`; everything we do not use is ignored), and the public [`Table`] is the
//! shape the UI draws.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
    process::{Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use serde_json::Value;

use crate::{history::format_duration, project::flatten_leaves};

const SHORT_REV_LEN: usize = 7;

// ---------------------------------------------------------------------------------------------
// The JSON as DVC writes it
// ---------------------------------------------------------------------------------------------

/// One revision: the workspace, a baseline commit, or (nested) an experiment.
#[derive(Debug, Deserialize)]
struct RawRevision {
    rev: String,
    name: Option<String>,
    data: Option<RawData>,
    error: Option<Value>,
    experiments: Option<Vec<RawExperiment>>,
}

/// An experiment may consist of several revisions (checkpoints); we show each one.
#[derive(Debug, Deserialize)]
struct RawExperiment {
    #[serde(default)]
    revs: Vec<RawRevision>,
}

#[derive(Debug, Deserialize)]
struct RawData {
    timestamp: Option<String>,
    params: Option<BTreeMap<String, RawFile>>,
    metrics: Option<BTreeMap<String, RawFile>>,
}

/// `{"data": {...}}` for a readable file, `{"error": {...}}` for a broken one.
#[derive(Debug, Deserialize)]
struct RawFile {
    data: Option<Value>,
}

// ---------------------------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Workspace,
    Baseline,
    Experiment,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: RowKind,
    /// The experiment or branch name, or a short commit id when there is no name.
    pub label: String,
    pub rev: String,
    pub timestamp: Option<String>,
    /// Column label (`file:dotted.key`) -> value, for the leaves of every tracked param file.
    pub params: BTreeMap<String, Value>,
    pub metrics: BTreeMap<String, Value>,
    /// Set when DVC could not read this revision.
    pub error: Option<String>,
    /// How long the experiment took to run, when this interface ran it (DVC does not record it).
    pub run_secs: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Column {
    Name,
    Param(String),
    Metric(String),
    /// How long the experiment took; shown only when at least one row has a time.
    Duration,
    /// How long ago the experiment was made (`3h ago`); shown when a row has a timestamp.
    Age,
    /// A series of metrics that differ only in a number (`m.json:by_fold.*.rmse`), drawn as one
    /// sparkline with the mean. The text is the metric's label with the number replaced by `*`.
    Spread(String),
}

/// Which kind of columns the table shows. Hundreds of params and metrics do not fit one table,
/// so they are looked at one kind at a time (`Tab` switches).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    Params,
    #[default]
    Metrics,
    All,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Params, Scope::Metrics, Scope::All];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Params => "Params",
            Scope::Metrics => "Metrics",
            Scope::All => "All",
        }
    }

    /// The next scope, or the previous one.
    pub fn cycle(self, forward: bool) -> Scope {
        let index = Scope::ALL.iter().position(|s| *s == self).unwrap_or(0);
        let len = Scope::ALL.len();
        Scope::ALL[(index + if forward { 1 } else { len - 1 }) % len]
    }
}

/// What decides which columns are on screen.
#[derive(Debug, Clone, Copy)]
pub struct ColumnQuery<'a> {
    pub scope: Scope,
    /// Params and metrics that were hidden by hand (`v` / `-`).
    pub hidden: &'a BTreeSet<String>,
    /// Keep the columns whose value is the same in every row.
    pub show_unchanged: bool,
    /// Words that must all appear in a column's label (case does not matter).
    pub filter: &'a str,
    /// Columns kept at the left whatever the scope, the filter or the unchanged ones say.
    pub pinned: &'a BTreeSet<String>,
    /// Draw a series of numbered metrics (`by_fold.0`, `by_fold.1`, ...) as one sparkline column.
    pub fold: bool,
}

/// Split a label `file:a.b.leaf` into its group (`file:a.b`, or just `file` when the key has no
/// dot) and the leaf. Params and metrics that sit side by side in a file share a group.
pub fn split_group(label: &str) -> (&str, &str) {
    let key_start = label.find(':').map_or(0, |colon| colon + 1);
    match label[key_start..].rfind('.') {
        Some(dot) => (&label[..key_start + dot], &label[key_start + dot + 1..]),
        None => (
            label[..key_start].trim_end_matches(':'),
            &label[key_start..],
        ),
    }
}

impl Column {
    /// The group a param or metric belongs to (`params.yaml:train`).
    pub fn group(&self) -> Option<&str> {
        match self {
            Column::Name | Column::Duration | Column::Age => None,
            Column::Param(label) | Column::Metric(label) | Column::Spread(label) => {
                Some(split_group(label).0)
            }
        }
    }

    /// The header text without the group, which is shown on a line of its own.
    pub fn short_title(&self) -> &str {
        match self {
            Column::Param(label) | Column::Metric(label) | Column::Spread(label) => {
                split_group(label).1
            }
            other => other.title(),
        }
    }

    /// The header text.
    pub fn title(&self) -> &str {
        match self {
            Column::Name => "name",
            Column::Duration => "run time",
            Column::Age => "age",
            Column::Param(label) | Column::Metric(label) | Column::Spread(label) => label,
        }
    }

    /// A text that names the column in the saved view (see [`Column::from_key`]).
    pub fn to_key(&self) -> String {
        match self {
            Column::Name => "name".to_owned(),
            Column::Duration => "duration".to_owned(),
            Column::Age => "age".to_owned(),
            Column::Param(label) => format!("param:{label}"),
            Column::Metric(label) => format!("metric:{label}"),
            Column::Spread(label) => format!("spread:{label}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Column> {
        Some(match key {
            "name" => Column::Name,
            "duration" => Column::Duration,
            "age" => Column::Age,
            _ => {
                let (kind, label) = key.split_once(':')?;
                match kind {
                    "param" => Column::Param(label.to_owned()),
                    "metric" => Column::Metric(label.to_owned()),
                    "spread" => Column::Spread(label.to_owned()),
                    _ => return None,
                }
            }
        })
    }
}

/// `m.json:by_fold.3.rmse` -> (`m.json:by_fold.*.rmse`, 3): the first all-digit part of a metric's
/// key is the number of the series.
pub fn series_key(label: &str) -> Option<(String, u64)> {
    let key_start = label.find(':').map_or(0, |colon| colon + 1);
    let mut offset = key_start;
    for part in label[key_start..].split('.') {
        if !part.is_empty()
            && part.len() <= 9
            && part.bytes().all(|b| b.is_ascii_digit())
            && let Ok(number) = part.parse()
        {
            let mut key = label[..offset].to_owned();
            key.push('*');
            key.push_str(&label[offset + part.len()..]);
            return Some((key, number));
        }
        offset += part.len() + 1;
    }
    None
}

/// The values of the series `key` in `row`, in the order of their numbers.
pub fn series_values(row: &Row, key: &str) -> Vec<f64> {
    let mut found: Vec<(u64, f64)> = row
        .metrics
        .iter()
        .filter_map(|(label, value)| {
            let (found_key, number) = series_key(label)?;
            (found_key == key).then_some(())?;
            Some((number, value.as_f64().filter(|v| v.is_finite())?))
        })
        .collect();
    found.sort_by_key(|(number, _)| *number);
    found.into_iter().map(|(_, value)| value).collect()
}

const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// `▂▃▅█▆ 0.123`: the values on their own scale, then their mean.
pub fn sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let (low, high) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        });
    let bars: String = values
        .iter()
        .map(|v| {
            if high > low {
                BLOCKS[(((v - low) / (high - low)) * 7.0).round() as usize]
            } else {
                BLOCKS[3]
            }
        })
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    format!("{bars} {}", display(&Value::from(mean)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// Workspace first, then each baseline followed by its experiments.
    pub rows: Vec<Row>,
    /// Union of param labels across all rows, sorted.
    pub param_columns: Vec<String>,
    pub metric_columns: Vec<String>,
    /// Full commit id -> the message given with `dvc exp run -m` (see [`crate::messages`]).
    pub messages: BTreeMap<String, String>,
    /// Which way metrics improve where the name does not say (or says otherwise): a metric label,
    /// or part of its name in lower case, to the direction. Set by the config file and `B`.
    pub directions: BTreeMap<String, Better>,
}

/// The bare words Python's `json` writes for floats that JSON cannot hold.
const NON_FINITE: [&str; 3] = ["NaN", "-Infinity", "Infinity"];

/// DVC writes `NaN` and `Infinity` as bare words (Python's `json` does), which is not JSON and
/// which `serde_json` refuses. Quote them outside strings, so they show as text in the table
/// (a string is no number, so sorting and deltas skip it).
fn quote_non_finite(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut rest = json;
    while let Some(c) = rest.chars().next() {
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c == '"' {
            in_string = true;
        } else if let Some(word) = NON_FINITE.iter().find(|word| rest.starts_with(**word)) {
            out.push_str(&format!("\"{word}\""));
            rest = &rest[word.len()..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Parse the output of `dvc exp show --json`.
pub fn parse(json: &str) -> Result<Table, serde_json::Error> {
    let revisions: Vec<RawRevision> = serde_json::from_str(&quote_non_finite(json))?;
    let mut rows = Vec::new();
    for (position, revision) in revisions.into_iter().enumerate() {
        // The workspace comes first and has no commit name.
        let kind = if position == 0 && revision.name.is_none() && revision.rev == "workspace" {
            RowKind::Workspace
        } else {
            RowKind::Baseline
        };
        let nested = revision.experiments.unwrap_or_default();
        rows.push(row_from(
            kind,
            revision.rev,
            revision.name,
            revision.data,
            revision.error,
        ));
        for experiment in nested {
            for rev in experiment.revs {
                rows.push(row_from(
                    RowKind::Experiment,
                    rev.rev,
                    rev.name,
                    rev.data,
                    rev.error,
                ));
            }
        }
    }

    let columns = |pick: fn(&Row) -> &BTreeMap<String, Value>| -> Vec<String> {
        let labels: BTreeSet<&String> = rows.iter().flat_map(|row| pick(row).keys()).collect();
        labels.into_iter().cloned().collect()
    };
    let param_columns = columns(|row| &row.params);
    let metric_columns = columns(|row| &row.metrics);
    Ok(Table {
        rows,
        param_columns,
        metric_columns,
        messages: BTreeMap::new(),
        directions: BTreeMap::new(),
    })
}

fn row_from(
    kind: RowKind,
    rev: String,
    name: Option<String>,
    data: Option<RawData>,
    error: Option<Value>,
) -> Row {
    let label = match (kind, name) {
        (RowKind::Workspace, _) => "workspace".to_owned(),
        (_, Some(name)) => name,
        (_, None) => rev.chars().take(SHORT_REV_LEN).collect(),
    };
    let data = data.unwrap_or(RawData {
        timestamp: None,
        params: None,
        metrics: None,
    });
    Row {
        kind,
        label,
        rev,
        timestamp: data.timestamp,
        params: leaves_by_file(data.params.unwrap_or_default()),
        metrics: leaves_by_file(data.metrics.unwrap_or_default()),
        error: error.as_ref().map(error_text),
        run_secs: None,
    }
}

/// `{"params.json": {"data": {"download": {"limit": 3}}}}` -> `{"params.json:download.limit": 3}`.
fn leaves_by_file(files: BTreeMap<String, RawFile>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (file, raw) in files {
        let Some(Value::Object(tree)) = raw.data else {
            continue;
        };
        for (key, value) in &tree {
            for (leaf_key, leaf) in flatten_leaves(key, value) {
                out.insert(format!("{file}:{leaf_key}"), leaf.clone());
            }
        }
    }
    out
}

fn error_text(error: &Value) -> String {
    error
        .get("msg")
        .and_then(Value::as_str)
        .map_or_else(|| error.to_string(), str::to_owned)
}

impl Table {
    /// `name`, then every param column, then every metric column.
    pub fn columns(&self) -> Vec<Column> {
        self.columns_for(&ColumnQuery {
            scope: Scope::All,
            hidden: &BTreeSet::new(),
            show_unchanged: true,
            filter: "",
            pinned: &BTreeSet::new(),
            fold: false,
        })
    }

    /// Does `column` hold different values in different rows? (Always true with fewer than two
    /// rows: there is nothing to compare, and an empty table would be no help.)
    fn varies(&self, column: &Column) -> bool {
        if self.rows.len() < 2 {
            return true;
        }
        let mut texts = self.rows.iter().map(|row| cell_text(row, column));
        let first = texts.next();
        texts.any(|text| Some(&text) != first.as_ref())
    }

    /// The columns on screen: the name, then the params and metrics the query lets through.
    pub fn columns_for(&self, query: &ColumnQuery) -> Vec<Column> {
        let words: Vec<String> = query
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let wanted = |label: &str| {
            let label = label.to_lowercase();
            words.iter().all(|word| label.contains(word))
        };
        let keep = |column: &Column| query.show_unchanged || self.varies(column);
        let params = matches!(query.scope, Scope::Params | Scope::All);
        let metrics = matches!(query.scope, Scope::Metrics | Scope::All);
        let pinned = |label: &str| query.pinned.contains(label);
        // The series worth folding: three or more metrics that differ only in a number.
        let mut series: BTreeMap<String, usize> = BTreeMap::new();
        if query.fold {
            for label in &self.metric_columns {
                if let Some((key, _)) = series_key(label) {
                    *series.entry(key).or_default() += 1;
                }
            }
            series.retain(|_, count| *count >= 3);
        }
        let mut params_out: Vec<Column> = Vec::new();
        let mut metrics_out: Vec<Column> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for label in &self.param_columns {
            let column = Column::Param(label.clone());
            if query.hidden.contains(label) {
                continue;
            }
            if pinned(label) || (params && wanted(label) && keep(&column)) {
                params_out.push(column);
            }
        }
        for label in &self.metric_columns {
            let folded = series_key(label).filter(|(key, _)| series.contains_key(key));
            let (column, shown_label) = match folded {
                Some((key, _)) => {
                    if !seen.insert(key.clone()) {
                        continue;
                    }
                    (Column::Spread(key.clone()), key)
                }
                None => (Column::Metric(label.clone()), label.clone()),
            };
            if query.hidden.contains(&shown_label) {
                continue;
            }
            if pinned(&shown_label) || (metrics && wanted(&shown_label) && keep(&column)) {
                metrics_out.push(column);
            }
        }
        let is_pinned = |column: &Column| match column {
            Column::Param(label) | Column::Metric(label) | Column::Spread(label) => pinned(label),
            _ => false,
        };
        let (pinned_columns, rest): (Vec<Column>, Vec<Column>) = params_out
            .into_iter()
            .chain(metrics_out)
            .partition(|column| is_pinned(column));
        std::iter::once(Column::Name)
            // Pinned columns stay on screen at the left, however far the table is scrolled.
            .chain(pinned_columns)
            // Next to the name, which is always on screen; at the far right it would scroll out of sight.
            .chain(
                self.rows
                    .iter()
                    .any(|row| row.run_secs.is_some())
                    .then_some(Column::Duration),
            )
            .chain(
                self.rows
                    .iter()
                    .any(|row| row.timestamp.is_some())
                    .then_some(Column::Age),
            )
            .chain(rest)
            .collect()
    }

    pub fn has_experiments(&self) -> bool {
        self.rows.iter().any(|row| row.kind == RowKind::Experiment)
    }

    /// Rows in display order. Only experiments are reordered, and only within the run of
    /// experiments that belongs to one baseline; the workspace and baselines stay put.
    pub fn ordered(&self, sort: &Sort) -> Vec<&Row> {
        let shown = |row: &Row| {
            row.kind != RowKind::Experiment
                || sort
                    .only
                    .as_ref()
                    .is_none_or(|only| only.contains(&row.rev))
        };
        let mut out: Vec<&Row> = Vec::with_capacity(self.rows.len());
        let mut run: Vec<&Row> = Vec::new();
        for row in &self.rows {
            if row.kind == RowKind::Experiment {
                if shown(row) {
                    run.push(row);
                }
            } else {
                flush_run(&mut run, &mut out, sort.by.as_ref());
                out.push(row);
            }
        }
        flush_run(&mut run, &mut out, sort.by.as_ref());
        out
    }

    /// The commit ids of the rows whose cell in `column` reads the same as `row`'s.
    pub fn similar_rows(&self, column: &Column, row: &Row) -> BTreeSet<String> {
        let wanted = cell_text(row, column);
        self.rows
            .iter()
            .filter(|other| cell_text(other, column) == wanted)
            .map(|other| other.rev.clone())
            .collect()
    }
}

/// Sort a run of experiments and move it to the end of `out`.
/// The named lifetime `'a` says both vectors hold references into the same table; a closure
/// cannot state that, which is why this is a function.
fn flush_run<'a>(run: &mut Vec<&'a Row>, out: &mut Vec<&'a Row>, by: Option<&(Column, Direction)>) {
    if let Some((column, direction)) = by {
        // `sort_by` is stable: rows that compare equal keep the order DVC gave them.
        run.sort_by(|a, b| compare(sort_value(a, column), sort_value(b, column), *direction));
    }
    out.append(run);
}

/// The text shown in a cell; empty when the row has no such value.
pub fn cell_text(row: &Row, column: &Column) -> String {
    match column {
        Column::Name => row.label.clone(),
        Column::Param(label) => row.params.get(label).map(display).unwrap_or_default(),
        Column::Metric(label) => row.metrics.get(label).map(display).unwrap_or_default(),
        Column::Spread(key) => sparkline(&series_values(row, key)),
        Column::Duration => row
            .run_secs
            .map(|secs| format_duration(Duration::from_secs_f64(secs)))
            .unwrap_or_default(),
        Column::Age => row
            .timestamp
            .as_deref()
            .and_then(local_seconds)
            .map(|then| ago(then, now_local_seconds()))
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------------------------
// How long ago
// ---------------------------------------------------------------------------------------------

/// DVC writes the commit time as local wall-clock time (`2026-11-09T14:04:13`), without a zone.
/// So the time is compared with the wall-clock time here, both as seconds since 1970-01-01 00:00
/// "as if UTC". (Across a change of summer time an age can be an hour off; that is no matter.)
fn local_seconds(timestamp: &str) -> Option<i64> {
    let digits = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since 1970-01-01 of a civil date (Howard Hinnant's `days_from_civil`).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// The offset of the local time zone from UTC in seconds, asked of `date` once.
fn local_offset_seconds() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let zone = Command::new("date")
            .arg("+%z")
            .stdin(Stdio::null())
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default();
        parse_zone_offset(&zone).unwrap_or(0)
    })
}

/// `+0100` -> 3600, `-0530` -> -19800.
fn parse_zone_offset(zone: &str) -> Option<i64> {
    let sign = match zone.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let hours: i64 = zone.get(1..3)?.parse().ok()?;
    let minutes: i64 = zone.get(3..5)?.parse().ok()?;
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn now_local_seconds() -> i64 {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    unix + local_offset_seconds()
}

/// `then` and `now` in the same clock: `just now`, `5m ago`, `3h ago`, `2d ago`, `3w ago`, `4mo ago`.
pub fn ago(then: i64, now: i64) -> String {
    let secs = (now - then).max(0);
    let (minute, hour, day) = (60, 3_600, 86_400);
    match secs {
        s if s < minute => "just now".to_owned(),
        s if s < hour => format!("{}m ago", s / minute),
        s if s < day => format!("{}h ago", s / hour),
        s if s < 14 * day => format!("{}d ago", s / day),
        s if s < 60 * day => format!("{}w ago", s / (7 * day)),
        s if s < 730 * day => format!("{}mo ago", s / (30 * day)),
        s => format!("{}y ago", s / (365 * day)),
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Focusing on one experiment
// ---------------------------------------------------------------------------------------------

/// Which way a metric improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Higher,
    Lower,
}

impl Better {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "lower" | "low" | "min" | "down" => Some(Self::Lower),
            "higher" | "high" | "max" | "up" => Some(Self::Higher),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Higher => "higher",
            Self::Lower => "lower",
        }
    }
}

/// Words in a metric's name that say lower is better / higher is better. The second list is asked
/// first: `mae_skill` improves upwards although `mae` improves downwards.
const LOWER_IS_BETTER: &[&str] = &[
    "loss",
    "error",
    "err",
    "mae",
    "mse",
    "rmse",
    "mape",
    "smape",
    "crps",
    "rhat",
    "divergen",
    "nll",
    "perplexity",
    "deviance",
    "bias",
    "latency",
    "duration",
];
const HIGHER_IS_BETTER: &[&str] = &[
    "skill",
    "auc",
    "accuracy",
    "acc",
    "f1",
    "precision",
    "recall",
    "r2",
    "elpd",
    "ess",
    "score",
    "iou",
];

/// The direction of improvement of the metric `label`, when its name tells. Names that tell
/// nothing (`coverage_50`: the aim is a value, not a direction) have none, and no row is ever
/// "the best" in them.
pub fn better_direction(label: &str) -> Option<Better> {
    let leaf = split_group(label).1.to_lowercase();
    let words: Vec<&str> = leaf
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // A word is a known one, or (for the longer ones) starts with it: `divergences`.
    let has = |list: &[&str]| {
        words.iter().any(|word| {
            list.iter()
                .any(|known| word == known || (known.len() >= 4 && word.starts_with(known)))
        })
    };
    if has(HIGHER_IS_BETTER) {
        Some(Better::Higher)
    } else if has(LOWER_IS_BETTER) {
        Some(Better::Lower)
    } else {
        None
    }
}

/// Is `row`'s value of the metric `label` the best of all rows (ties count)? Needs a known
/// direction and at least two numbers to compare.
pub fn is_best(table: &Table, row: &Row, label: &str) -> bool {
    let Some(better) = table.direction_of(label) else {
        return false;
    };
    let Some(mine) = row.metrics.get(label).and_then(Value::as_f64) else {
        return false;
    };
    let numbers: Vec<f64> = table
        .rows
        .iter()
        .filter_map(|other| other.metrics.get(label).and_then(Value::as_f64))
        .filter(|number| number.is_finite())
        .collect();
    if numbers.len() < 2 || !mine.is_finite() {
        return false;
    }
    let best = numbers.iter().copied().fold(
        match better {
            Better::Higher => f64::NEG_INFINITY,
            Better::Lower => f64::INFINITY,
        },
        |best, number| match better {
            Better::Higher => best.max(number),
            Better::Lower => best.min(number),
        },
    );
    mine == best
}

impl Table {
    /// Which way the metric improves: what was set for it (its label, else the longest part of
    /// its name that was given), else what its name says.
    pub fn direction_of(&self, label: &str) -> Option<Better> {
        if let Some(better) = self.directions.get(label) {
            return Some(*better);
        }
        let lower = label.to_lowercase();
        self.directions
            .iter()
            .filter(|(part, _)| lower.contains(part.as_str()))
            .max_by_key(|(part, _)| part.len())
            .map(|(_, better)| *better)
            .or_else(|| better_direction(label))
    }

    /// The commit ids of the `count` best rows (ties may add more) in the metric `label`;
    /// empty when the direction is unknown.
    pub fn best_rows(&self, label: &str, count: usize) -> Vec<String> {
        let Some(better) = self.direction_of(label) else {
            return Vec::new();
        };
        let mut scored: Vec<(f64, &Row)> = self
            .rows
            .iter()
            .filter_map(|row| {
                let number = row.metrics.get(label)?.as_f64()?;
                number.is_finite().then_some((number, row))
            })
            .collect();
        scored.sort_by(|a, b| match better {
            Better::Higher => b.0.total_cmp(&a.0),
            Better::Lower => a.0.total_cmp(&b.0),
        });
        let Some(&(cutoff, _)) = scored.get(count.saturating_sub(1)) else {
            return scored.iter().map(|(_, row)| row.rev.clone()).collect();
        };
        scored
            .iter()
            .take_while(|(number, _)| match better {
                Better::Higher => *number >= cutoff,
                Better::Lower => *number <= cutoff,
            })
            .map(|(_, row)| row.rev.clone())
            .collect()
    }
}

/// The rows as CSV: a header of the full column labels, then a line per row.
pub fn csv_text(rows: &[&Row], columns: &[Column]) -> String {
    fn field(text: &str) -> String {
        if text.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", text.replace('"', "\"\""))
        } else {
            text.to_owned()
        }
    }
    let mut out = columns
        .iter()
        .map(|column| field(column.title()))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for row in rows {
        let line: Vec<String> = columns
            .iter()
            .map(|column| field(&cell_text(row, column)))
            .collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

/// One param or metric that differs between two rows.
#[derive(Debug, Clone, PartialEq)]
pub struct CompareField {
    pub column: Column,
    pub a: String,
    pub b: String,
    /// How `b` moved from `a` (numbers only).
    pub delta: Option<(Change, String)>,
    /// Which row is better in this metric, when the direction is known: `Some(true)` is `a`.
    pub a_better: Option<bool>,
}

/// The params and metrics of `columns` in which `a` and `b` differ.
pub fn compare_fields(table: &Table, a: &Row, b: &Row, columns: &[Column]) -> Vec<CompareField> {
    columns
        .iter()
        .filter_map(|column| {
            let (Column::Param(label) | Column::Metric(label)) = column else {
                return None;
            };
            let (left, right) = (cell_text(a, column), cell_text(b, column));
            if left == right {
                return None;
            }
            let (delta, a_better) = match column {
                Column::Metric(_) => (
                    metric_delta(b, a, label),
                    match (
                        a.metrics.get(label).and_then(Value::as_f64),
                        b.metrics.get(label).and_then(Value::as_f64),
                        table.direction_of(label),
                    ) {
                        (Some(x), Some(y), Some(Better::Higher)) if x != y => Some(x > y),
                        (Some(x), Some(y), Some(Better::Lower)) if x != y => Some(x < y),
                        _ => None,
                    },
                ),
                _ => (None, None),
            };
            Some(CompareField {
                column: column.clone(),
                a: left,
                b: right,
                delta,
                a_better,
            })
        })
        .collect()
}

/// One line of the single-experiment view.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusField {
    pub column: Column,
    pub value: String,
    /// The baseline's value, when the row has a baseline and the baseline has one.
    pub baseline: Option<String>,
    /// How a metric moved from the baseline (numbers only).
    pub delta: Option<(Change, String)>,
    /// A param that differs from the baseline's.
    pub differs: bool,
    /// The row is the best of all in this metric (see [`is_best`]).
    pub best: bool,
}

/// The params and metrics of `columns` that the row at `index` of `rows` has a value for.
pub fn focus_fields(
    table: &Table,
    rows: &[&Row],
    index: usize,
    columns: &[Column],
) -> Vec<FocusField> {
    let Some(row) = rows.get(index) else {
        return Vec::new();
    };
    let baseline = baseline_of(rows, index);
    columns
        .iter()
        .filter_map(|column| {
            let (Column::Param(label) | Column::Metric(label)) = column else {
                return None;
            };
            let value = cell_text(row, column);
            if value.is_empty() {
                return None;
            }
            let baseline_value = baseline
                .map(|baseline| cell_text(baseline, column))
                .filter(|text| !text.is_empty());
            Some(FocusField {
                column: column.clone(),
                differs: param_differs(row, baseline, column),
                delta: match column {
                    Column::Metric(_) => baseline.and_then(|b| metric_delta(row, b, label)),
                    _ => None,
                },
                best: matches!(column, Column::Metric(_)) && is_best(table, row, label),
                baseline: baseline_value,
                value,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Differences from the baseline
// ---------------------------------------------------------------------------------------------

/// The baseline an experiment was run from: the nearest baseline row above it in `rows`
/// (rows in display order). `None` for the workspace and baseline rows themselves.
pub fn baseline_of<'a>(rows: &[&'a Row], index: usize) -> Option<&'a Row> {
    if rows.get(index)?.kind != RowKind::Experiment {
        return None;
    }
    rows[..index]
        .iter()
        .rev()
        .find(|row| row.kind == RowKind::Baseline)
        .copied()
}

/// Does this experiment's param differ from its baseline's? (Baseline and workspace: never.)
pub fn param_differs(row: &Row, baseline: Option<&Row>, column: &Column) -> bool {
    let Some(baseline) = baseline else {
        return false;
    };
    matches!(column, Column::Param(_)) && cell_text(row, column) != cell_text(baseline, column)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Up,
    Down,
    Same,
}

/// The signed difference between an experiment's metric and its baseline's, written with as many
/// decimals as the more precise of the two numbers (so float noise like `0.18669999` never shows).
/// `None` when either side is missing or not a number.
pub fn metric_delta(row: &Row, baseline: &Row, label: &str) -> Option<(Change, String)> {
    let now = row.metrics.get(label)?.as_f64()?;
    let before = baseline.metrics.get(label)?.as_f64()?;
    let decimals = decimals(now).max(decimals(before));
    let difference = now - before;
    let text = format!("{difference:+.decimals$}");
    // A difference that rounds to zero at this precision is no difference at all.
    let change = if text
        .trim_start_matches(['+', '-'])
        .chars()
        .all(|c| matches!(c, '0' | '.'))
    {
        Change::Same
    } else if difference > 0.0 {
        Change::Up
    } else {
        Change::Down
    };
    let text = if change == Change::Same {
        "±0".to_owned()
    } else {
        text
    };
    Some((change, text))
}

/// Digits after the decimal point in the shortest text that reads back as `value`.
fn decimals(value: f64) -> usize {
    let text = value.to_string();
    text.split_once('.')
        .map_or(0, |(_, fraction)| fraction.len())
        .min(10)
}

// ---------------------------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

/// How the rows are ordered, and which experiments are shown at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sort {
    pub by: Option<(Column, Direction)>,
    /// When set, only the experiments with these commit ids are shown (the workspace and the
    /// baselines always are: experiments are compared with them).
    pub only: Option<BTreeSet<String>>,
}

impl Sort {
    /// A new column sorts ascending; choosing the same column again flips the direction.
    pub fn choose(&mut self, column: Column) {
        let direction = match &self.by {
            Some((current, Direction::Ascending)) if *current == column => Direction::Descending,
            _ => Direction::Ascending,
        };
        self.by = Some((column, direction));
    }

    pub fn direction_of(&self, column: &Column) -> Option<Direction> {
        match &self.by {
            Some((current, direction)) if current == column => Some(*direction),
            _ => None,
        }
    }
}

/// What we compare: numbers numerically, everything else as text.
#[derive(Debug, Clone, PartialEq)]
enum SortValue {
    Number(f64),
    Text(String),
}

fn sort_value(row: &Row, column: &Column) -> Option<SortValue> {
    let value = match column {
        Column::Name => return Some(SortValue::Text(row.label.clone())),
        Column::Duration => return row.run_secs.map(SortValue::Number),
        Column::Age => {
            let then = local_seconds(row.timestamp.as_deref()?)?;
            return Some(SortValue::Number(then as f64));
        }
        Column::Param(label) => row.params.get(label)?,
        Column::Metric(label) => row.metrics.get(label)?,
        Column::Spread(key) => {
            let values = series_values(row, key);
            if values.is_empty() {
                return None;
            }
            return Some(SortValue::Number(
                values.iter().sum::<f64>() / values.len() as f64,
            ));
        }
    };
    Some(match value.as_f64() {
        Some(number) => SortValue::Number(number),
        None => SortValue::Text(display(value)),
    })
}

/// Empty cells go last whichever way we sort; `direction` only affects cells that have a value.
fn compare(a: Option<SortValue>, b: Option<SortValue>, direction: Direction) -> Ordering {
    let ordering = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Greater,
        (Some(_), None) => return Ordering::Less,
        (Some(x), Some(y)) => match (x, y) {
            (SortValue::Number(x), SortValue::Number(y)) => x.total_cmp(&y),
            (SortValue::Number(_), SortValue::Text(_)) => Ordering::Less,
            (SortValue::Text(_), SortValue::Number(_)) => Ordering::Greater,
            (SortValue::Text(x), SortValue::Text(y)) => x.cmp(&y),
        },
    };
    match direction {
        Direction::Ascending => ordering,
        Direction::Descending => ordering.reverse(),
    }
}

// ---------------------------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------------------------

/// Run `dvc exp show --json -A` (all commits, not only the current one) in `root` and parse it. Blocking: call it from a background thread.
/// Every failure becomes a readable message for the UI.
pub fn fetch(dvc: &Path, root: &Path) -> Result<Table, String> {
    let output = run_to_end(dvc, root)
        .map_err(|error| format!("cannot start {}: {error}", dvc.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map_or("killed by a signal".to_owned(), |c| format!("exit {c}"));
        return Err(failure_message(&code, &stderr));
    }
    let mut table = parse(&String::from_utf8_lossy(&output.stdout))
        .map_err(|error| format!("could not read the output of `dvc exp show --json`: {error}"))?;
    let revs: Vec<&str> = table.rows.iter().map(|row| row.rev.as_str()).collect();
    table.messages = crate::messages::read_all(root, &revs);
    Ok(table)
}

impl Table {
    /// The commit message of `row`, when it has one.
    pub fn message_of(&self, row: &Row) -> Option<&str> {
        self.messages
            .get(&row.rev)
            .map(String::as_str)
            .filter(|message| !message.is_empty())
    }
}

/// DVC's own error, plus advice for the one failure that every newcomer hits.
fn failure_message(code: &str, stderr: &str) -> String {
    let mut message = format!("dvc exp show failed ({code}):\n{}", stderr.trim());
    if stderr.contains("Only supported for Git repositories") {
        message.push_str(
            "\n\nExperiments are stored in Git, so the project must be a Git repository:\n\
             run `git init`, then commit it (`git add -A && git commit -m init`).\n\
             To try the feature, build the demo copy: tests/fixtures/demo/make-git-demo.sh",
        );
    }
    message
}

fn run_to_end(dvc: &Path, root: &Path) -> io::Result<Output> {
    let mut command = Command::new(dvc);
    command
        .args(["exp", "show", "--json", "-A"])
        .current_dir(root)
        .stdin(Stdio::null());
    // A script written a moment ago can look "busy" to exec; it clears up almost immediately.
    for _ in 0..4 {
        match command.output() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
    command.output()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::json;

    use super::*;

    fn two() -> Table {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exp_show/two.json");
        parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The titles of the columns, without the age column (its text depends on the clock).
    fn titles(columns: &[Column]) -> Vec<&str> {
        columns
            .iter()
            .filter(|column| **column != Column::Age)
            .map(Column::title)
            .collect()
    }

    fn query<'a>(
        scope: Scope,
        show_unchanged: bool,
        filter: &'a str,
        hidden: &'a BTreeSet<String>,
    ) -> ColumnQuery<'a> {
        static NONE: BTreeSet<String> = BTreeSet::new();
        ColumnQuery {
            scope,
            hidden,
            show_unchanged,
            filter,
            pinned: &NONE,
            fold: false,
        }
    }

    #[test]
    fn scope_picks_params_or_metrics_and_unchanged_columns_can_be_left_out() {
        let table = two();
        let none = BTreeSet::new();
        let params = table.columns_for(&query(Scope::Params, true, "", &none));
        assert_eq!(
            titles(&params),
            [
                "name",
                "params.yaml:features.scale",
                "params.yaml:train.epochs",
                "params.yaml:train.lr"
            ]
        );
        let metrics = table.columns_for(&query(Scope::Metrics, true, "", &none));
        assert_eq!(
            titles(&metrics),
            ["name", "metrics.json:auc", "metrics.json:loss"]
        );
        // `features.scale` is 2.0 in every row.
        let varying = table.columns_for(&query(Scope::Params, false, "", &none));
        assert_eq!(
            titles(&varying),
            ["name", "params.yaml:train.epochs", "params.yaml:train.lr"]
        );
    }

    #[test]
    fn the_filter_wants_every_word_in_the_label_and_ignores_case() {
        let table = two();
        let none = BTreeSet::new();
        let found = table.columns_for(&query(Scope::All, true, "TRAIN lr", &none));
        assert_eq!(titles(&found), ["name", "params.yaml:train.lr"]);
        let nothing = table.columns_for(&query(Scope::All, true, "train auc", &none));
        assert_eq!(titles(&nothing), ["name"]);
    }

    #[test]
    fn a_single_row_has_nothing_to_compare_so_nothing_counts_as_unchanged() {
        let mut table = two();
        table.rows.truncate(1);
        let none = BTreeSet::new();
        let columns = table.columns_for(&query(Scope::Params, false, "", &none));
        assert_eq!(columns.len(), 4);
    }

    #[test]
    fn labels_split_into_a_group_and_the_key_inside_it() {
        assert_eq!(
            split_group("params.yaml:train.lr"),
            ("params.yaml:train", "lr")
        );
        assert_eq!(
            split_group("metrics.json:by_fold.0.mae"),
            ("metrics.json:by_fold.0", "mae"),
            "the leaf is the last part of the key"
        );
        assert_eq!(split_group("params.yaml:lr"), ("params.yaml", "lr"));
        assert_eq!(split_group("lr"), ("", "lr"));
    }

    #[test]
    fn timestamps_become_seconds_on_one_clock() {
        assert_eq!(local_seconds("1970-01-01T00:00:00"), Some(0));
        assert_eq!(local_seconds("1970-01-02T00:00:01"), Some(86_401));
        assert_eq!(local_seconds("2000-03-01T00:00:00"), Some(951_868_800));
        let (a, b) = (
            local_seconds("2026-11-09T14:04:13").unwrap(),
            local_seconds("2026-11-09T17:04:13").unwrap(),
        );
        assert_eq!(b - a, 3 * 3_600);
        assert_eq!(local_seconds("garbage"), None);
        assert_eq!(local_seconds("2026-13-09T14:04:13"), None);
    }

    #[test]
    fn ages_use_the_largest_sensible_unit() {
        let now = 1_000_000_000;
        let at = |secs: i64| ago(now - secs, now);
        assert_eq!(at(5), "just now");
        assert_eq!(
            at(-30),
            "just now",
            "a clock slightly ahead is not the future"
        );
        assert_eq!(at(5 * 60), "5m ago");
        assert_eq!(at(3 * 3_600 + 59), "3h ago");
        assert_eq!(at(2 * 86_400), "2d ago");
        assert_eq!(at(21 * 86_400), "3w ago");
        assert_eq!(at(120 * 86_400), "4mo ago");
        assert_eq!(at(800 * 86_400), "2y ago");
    }

    #[test]
    fn the_zone_offset_of_date_is_read() {
        assert_eq!(parse_zone_offset("+0100"), Some(3_600));
        assert_eq!(parse_zone_offset("-0530"), Some(-19_800));
        assert_eq!(parse_zone_offset(""), None);
    }

    #[test]
    fn the_age_column_is_shown_next_to_the_name_and_sorts_by_time() {
        let table = two();
        let columns = table.columns();
        assert_eq!(columns[1], Column::Age);
        assert!(cell_text(&table.rows[2], &Column::Age).ends_with("ago"));
        let mut sort = Sort::default();
        sort.choose(Column::Age);
        let order: Vec<&str> = table
            .ordered(&sort)
            .iter()
            .filter(|r| r.kind == RowKind::Experiment)
            .map(|r| r.label.as_str())
            .collect();
        let mut by_time = table
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Experiment)
            .collect::<Vec<_>>();
        by_time.sort_by_key(|r| r.timestamp.clone());
        assert_eq!(
            order,
            by_time.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            "ascending age column: the oldest first"
        );
    }

    #[test]
    fn the_name_of_a_metric_tells_which_way_it_improves() {
        let dir = |label| better_direction(label);
        assert_eq!(dir("metrics.json:loss"), Some(Better::Lower));
        assert_eq!(dir("m.json:by_fold.0.mae"), Some(Better::Lower));
        assert_eq!(dir("m.json:max_rhat"), Some(Better::Lower));
        assert_eq!(dir("m.json:n_divergences"), Some(Better::Lower));
        assert_eq!(dir("m.json:auc"), Some(Better::Higher));
        assert_eq!(dir("m.json:min_ess_bulk"), Some(Better::Higher));
        assert_eq!(dir("m.json:elpd_per_cell"), Some(Better::Higher));
        assert_eq!(
            dir("m.json:mae_skill"),
            Some(Better::Higher),
            "skill beats mae"
        );
        assert_eq!(dir("m.json:crps_skill"), Some(Better::Higher));
        assert_eq!(
            dir("m.json:coverage_50"),
            None,
            "the aim is a value, not a direction"
        );
    }

    #[test]
    fn the_best_row_in_a_metric_is_found_with_ties_and_only_when_comparable() {
        let table = two();
        let by_label = |label: &str| table.rows.iter().find(|r| r.label == label).unwrap();
        // auc improves upwards: workspace and lr-010 tie at 0.9356.
        assert!(is_best(&table, by_label("lr-010"), "metrics.json:auc"));
        assert!(is_best(&table, by_label("workspace"), "metrics.json:auc"));
        assert!(!is_best(&table, by_label("lr-005"), "metrics.json:auc"));
        // loss improves downwards.
        assert!(is_best(&table, by_label("lr-010"), "metrics.json:loss"));
        assert!(!is_best(&table, by_label("master"), "metrics.json:loss"));
        // Nothing to compare, or no known direction: no crown.
        assert!(!is_best(&table, by_label("lr-010"), "metrics.json:missing"));
        let mut one = two();
        one.rows.truncate(1);
        assert!(!is_best(&one, &one.rows[0], "metrics.json:auc"));
    }

    #[test]
    fn a_set_direction_beats_the_name_and_longest_part_wins() {
        let mut table = two();
        assert_eq!(table.direction_of("metrics.json:auc"), Some(Better::Higher));
        table.directions.insert("auc".into(), Better::Lower);
        assert_eq!(table.direction_of("metrics.json:auc"), Some(Better::Lower));
        table
            .directions
            .insert("metrics.json:auc".into(), Better::Higher);
        assert_eq!(table.direction_of("metrics.json:auc"), Some(Better::Higher));
        // A name that tells nothing gets one from the setting.
        assert_eq!(table.direction_of("m.json:coverage_50"), None);
        table.directions.insert("coverage".into(), Better::Higher);
        assert_eq!(
            table.direction_of("m.json:coverage_50"),
            Some(Better::Higher)
        );
        // The crown follows it.
        table.directions.insert("auc".into(), Better::Lower);
        table.directions.remove("metrics.json:auc");
        let lr005 = table.rows.iter().find(|r| r.label == "lr-005").unwrap();
        assert!(!is_best(&table, lr005, "metrics.json:auc"));
    }

    #[test]
    fn the_best_rows_are_ranked_in_the_direction_of_the_metric() {
        let table = two();
        let label_of = |revs: Vec<String>| -> Vec<String> {
            let mut names: Vec<String> = revs
                .iter()
                .map(|rev| {
                    table
                        .rows
                        .iter()
                        .find(|r| &r.rev == rev)
                        .unwrap()
                        .label
                        .clone()
                })
                .collect();
            names.sort();
            names
        };
        // auc: workspace and lr-010 tie at the top.
        assert_eq!(
            label_of(table.best_rows("metrics.json:auc", 1)),
            ["lr-010", "workspace"]
        );
        assert_eq!(table.best_rows("metrics.json:auc", 10).len(), 4);
        assert!(table.best_rows("metrics.json:missing", 3).is_empty());
    }

    #[test]
    fn compare_lists_only_the_fields_that_differ_and_who_is_ahead() {
        let table = two();
        let row = |label: &str| table.rows.iter().find(|r| r.label == label).unwrap();
        let fields = compare_fields(&table, row("lr-005"), row("lr-010"), &table.columns());
        assert!(!fields.is_empty());
        assert!(fields.iter().all(|f| f.a != f.b));
        let auc = fields
            .iter()
            .find(|f| f.column == Column::Metric("metrics.json:auc".into()))
            .unwrap();
        assert_eq!(auc.a_better, Some(false), "lr-010 has the higher auc");
        assert!(auc.delta.is_some());
        let same = compare_fields(&table, row("lr-010"), row("lr-010"), &table.columns());
        assert!(same.is_empty());
    }

    fn folds_table() -> Table {
        let json = r#"[
          {"rev": "workspace", "name": null, "data": {"timestamp": null, "params": {},
            "metrics": {"m.json": {"data": {"by_fold": {"0": {"rmse": 1.0}, "1": {"rmse": 3.0},
              "2": {"rmse": 2.0}}, "loss": 0.5}}}}},
          {"rev": "b", "name": "other", "data": {"timestamp": null, "params": {},
            "metrics": {"m.json": {"data": {"by_fold": {"0": {"rmse": 2.0}, "1": {"rmse": 2.0},
              "2": {"rmse": 2.0}}, "loss": 0.7}}}}}
        ]"#;
        parse(json).unwrap()
    }

    #[test]
    fn numbered_metrics_are_one_sparkline_column_when_folded() {
        assert_eq!(
            series_key("m.json:by_fold.12.rmse"),
            Some(("m.json:by_fold.*.rmse".to_owned(), 12))
        );
        assert_eq!(series_key("m.json:loss"), None);
        let table = folds_table();
        let none = BTreeSet::new();
        let mut query = query(Scope::Metrics, true, "", &none);
        let unfolded = table.columns_for(&query);
        assert_eq!(unfolded.len(), 1 + 4);
        query.fold = true;
        let folded = table.columns_for(&query);
        assert_eq!(
            folded,
            [
                Column::Name,
                Column::Spread("m.json:by_fold.*.rmse".into()),
                Column::Metric("m.json:loss".into())
            ]
        );
        let spread = &folded[1];
        assert_eq!(cell_text(&table.rows[0], spread), "▁█▅ 2.0");
        assert_eq!(cell_text(&table.rows[1], spread), "▄▄▄ 2.0");
        assert_eq!(spread.group(), Some("m.json:by_fold.*"));
        assert_eq!(spread.short_title(), "rmse");
        assert_eq!(Column::from_key(&spread.to_key()), Some(spread.clone()));
    }

    #[test]
    fn pinned_columns_come_first_and_ignore_scope_and_filter() {
        let table = two();
        let none = BTreeSet::new();
        let pinned: BTreeSet<String> = ["params.yaml:train.lr".to_owned()].into();
        let mut q = query(Scope::Metrics, true, "auc", &none);
        q.pinned = &pinned;
        let columns = table.columns_for(&q);
        assert_eq!(columns[0], Column::Name);
        assert_eq!(columns[1], Column::Param("params.yaml:train.lr".into()));
        assert!(columns.contains(&Column::Metric("metrics.json:auc".into())));
        assert!(!columns.contains(&Column::Metric("metrics.json:loss".into())));
        // Hiding wins over pinning.
        let hidden: BTreeSet<String> = ["params.yaml:train.lr".to_owned()].into();
        let mut q = query(Scope::Metrics, true, "", &hidden);
        q.pinned = &pinned;
        assert!(
            !table
                .columns_for(&q)
                .contains(&Column::Param("params.yaml:train.lr".into()))
        );
    }

    #[test]
    fn csv_has_a_header_a_line_per_row_and_quotes_what_needs_it() {
        let mut table = two();
        table.rows[0].label = "a,\"b\"".into();
        let rows: Vec<&Row> = table.rows.iter().collect();
        let columns = [Column::Name, Column::Metric("metrics.json:auc".into())];
        let csv = csv_text(&rows, &columns);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "name,metrics.json:auc");
        assert_eq!(lines.len(), 1 + table.rows.len());
        assert!(lines[1].starts_with("\"a,\"\"b\"\"\","), "{}", lines[1]);
    }

    #[test]
    fn focus_fields_list_what_the_row_has_with_its_baseline_and_crowns() {
        let table = two();
        let rows = table.ordered(&Sort::default());
        let index = rows.iter().position(|r| r.label == "lr-010").unwrap();
        let fields = focus_fields(&table, &rows, index, &table.columns());
        let lr = fields
            .iter()
            .find(|f| f.column == Column::Param("params.yaml:train.lr".into()))
            .unwrap();
        assert_eq!(
            (lr.value.as_str(), lr.baseline.as_deref()),
            ("0.1", Some("0.01"))
        );
        assert!(lr.differs && !lr.best);
        let auc = fields
            .iter()
            .find(|f| f.column == Column::Metric("metrics.json:auc".into()))
            .unwrap();
        assert!(auc.best);
        assert!(matches!(&auc.delta, Some((Change::Up, text)) if text.starts_with('+')));
        // Name and age are not fields.
        assert!(
            fields
                .iter()
                .all(|f| !matches!(f.column, Column::Name | Column::Age))
        );
    }

    #[test]
    fn similar_rows_share_a_cell_value_and_the_row_filter_keeps_baselines() {
        let table = two();
        let row = table.rows.iter().find(|r| r.label == "lr-010").unwrap();
        let column = Column::Param("params.yaml:train.lr".into());
        let similar = table.similar_rows(&column, row);
        let labels: BTreeSet<&str> = table
            .rows
            .iter()
            .filter(|r| similar.contains(&r.rev))
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, BTreeSet::from(["workspace", "lr-010"]));

        let sort = Sort {
            only: Some(similar),
            ..Sort::default()
        };
        let shown: Vec<&str> = table
            .ordered(&sort)
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            shown,
            ["workspace", "master", "lr-010"],
            "lr-005 is filtered out"
        );
    }

    #[test]
    fn nan_and_infinity_become_text_and_strings_are_left_alone() {
        let quoted = quote_non_finite(
            r#"{"a": NaN, "b": [-Infinity, Infinity], "c": "NaN \" Infinity", "d": 1.5}"#,
        );
        assert_eq!(
            quoted,
            r#"{"a": "NaN", "b": ["-Infinity", "Infinity"], "c": "NaN \" Infinity", "d": 1.5}"#
        );
        assert!(serde_json::from_str::<serde_json::Value>(&quoted).is_ok());
    }

    #[test]
    fn a_nan_metric_does_not_stop_the_table_from_loading() {
        let json = r#"[{"rev": "workspace", "name": null, "data": {"timestamp": null,
            "params": {}, "metrics": {"m.json": {"data": {"max_rhat": NaN, "loss": 0.5}}}}}]"#;
        let table = parse(json).expect("NaN is accepted");
        let metrics = &table.rows[0].metrics;
        assert_eq!(metrics["m.json:max_rhat"], json!("NaN"));
        assert_eq!(metrics["m.json:loss"], json!(0.5));
    }

    #[test]
    fn non_git_failure_explains_how_to_get_experiments() {
        let stderr = "ERROR: failed to show experiments - Only supported for Git repositories.";
        let message = failure_message("exit 1", stderr);
        assert!(
            message.starts_with("dvc exp show failed (exit 1):"),
            "{message}"
        );
        assert!(
            message.contains("git init") && message.contains("make-git-demo.sh"),
            "{message}"
        );
    }

    #[test]
    fn other_failures_get_no_git_advice() {
        let message = failure_message("exit 255", "ERROR: something else");
        assert!(!message.contains("git init"), "{message}");
    }

    fn sample(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show")
            .join(name);
        fs::read_to_string(path).unwrap()
    }

    fn labels(rows: &[&Row]) -> Vec<String> {
        rows.iter().map(|r| r.label.clone()).collect()
    }

    fn row<'a>(table: &'a Table, label: &str) -> &'a Row {
        table.rows.iter().find(|r| r.label == label).unwrap()
    }

    // ---- 7.1 parsing and the table model ----

    #[test]
    fn real_output_with_two_experiments() {
        let table = parse(&sample("two.json")).unwrap();
        let labels: Vec<_> = table
            .rows
            .iter()
            .map(|r| (r.kind, r.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                (RowKind::Workspace, "workspace"),
                (RowKind::Baseline, "master"),
                (RowKind::Experiment, "lr-010"),
                (RowKind::Experiment, "lr-005"),
            ]
        );
        assert!(table.has_experiments());
        assert_eq!(
            table.param_columns,
            [
                "params.yaml:features.scale",
                "params.yaml:train.epochs",
                "params.yaml:train.lr"
            ]
        );
        assert_eq!(
            table.metric_columns,
            ["metrics.json:auc", "metrics.json:loss"]
        );

        let fast = row(&table, "lr-010");
        let lr = Column::Param("params.yaml:train.lr".into());
        let auc = Column::Metric("metrics.json:auc".into());
        assert_eq!(cell_text(fast, &lr), "0.1");
        assert_eq!(cell_text(fast, &auc), "0.9356");
        assert_eq!(cell_text(row(&table, "master"), &auc), "0.6633");
        assert!(fast.timestamp.is_some());
        assert_eq!(row(&table, "workspace").timestamp, None);
    }

    #[test]
    fn without_experiments_only_workspace_and_baseline_remain() {
        let table = parse(&sample("none.json")).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert!(!table.has_experiments());
        assert_eq!(table.rows[0].kind, RowKind::Workspace);
    }

    #[test]
    fn columns_start_with_name_then_params_then_metrics() {
        let table = parse(&sample("two.json")).unwrap();
        let columns = table.columns();
        assert_eq!(columns[0], Column::Name);
        assert_eq!(columns[1], Column::Age, "right after the name");
        assert_eq!(columns.len(), 1 + 1 + 3 + 2);
        assert!(matches!(columns[2], Column::Param(_)));
        assert!(matches!(columns[5], Column::Metric(_)));
        assert_eq!(columns[2].title(), "params.yaml:features.scale");
    }

    #[test]
    fn nested_params_are_labelled_file_colon_dotted_key() {
        let json = json!([{"rev": "workspace", "name": null, "error": null, "experiments": null,
            "data": {"timestamp": null, "metrics": {}, "params": {
                "params.json": {"data": {"download": {"page_size": 100, "limit": null}}}}}}]);
        let table = parse(&json.to_string()).unwrap();
        assert_eq!(
            table.param_columns,
            [
                "params.json:download.limit",
                "params.json:download.page_size"
            ]
        );
        let ws = &table.rows[0];
        assert_eq!(
            cell_text(ws, &Column::Param("params.json:download.page_size".into())),
            "100"
        );
        assert_eq!(
            cell_text(ws, &Column::Param("params.json:download.limit".into())),
            "null"
        );
    }

    #[test]
    fn no_metrics_anywhere_means_no_metric_columns_and_no_failure() {
        // The shape of a project that tracks params but no metrics: `metrics: {}`.
        let json = json!([{"rev": "workspace", "name": null, "error": null, "experiments": null,
            "data": {"timestamp": null, "metrics": {}, "params": {"p.yaml": {"data": {"a": 1}}}}}]);
        let table = parse(&json.to_string()).unwrap();
        assert!(table.metric_columns.is_empty());
        assert_eq!(table.columns().len(), 2);
    }

    #[test]
    fn rows_lacking_a_key_get_an_empty_cell() {
        let json = json!([
            {"rev": "workspace", "name": null, "error": null, "experiments": null,
             "data": {"timestamp": null, "metrics": {}, "params": {"a.yaml": {"data": {"x": 1}}}}},
            {"rev": "abcdef1234", "name": "main", "error": null, "experiments": null,
             "data": {"timestamp": "t", "metrics": {}, "params": {"b.yaml": {"data": {"y": "hi"}}}}}
        ]);
        let table = parse(&json.to_string()).unwrap();
        assert_eq!(table.param_columns, ["a.yaml:x", "b.yaml:y"]);
        let x = Column::Param("a.yaml:x".into());
        assert_eq!(cell_text(&table.rows[1], &x), "");
        assert_eq!(
            cell_text(&table.rows[1], &Column::Param("b.yaml:y".into())),
            "hi"
        );
    }

    #[test]
    fn unnamed_revisions_use_a_short_commit_id() {
        let json = json!([{"rev": "0123456789abcdef", "name": null, "error": null, "experiments": null, "data": null}]);
        let table = parse(&json.to_string()).unwrap();
        assert_eq!(table.rows[0].label, "0123456");
    }

    #[test]
    fn a_broken_revision_keeps_its_error_instead_of_failing_everything() {
        let json = json!([
            {"rev": "workspace", "name": null, "error": null, "experiments": null, "data": {"timestamp": null, "params": {}, "metrics": {}}},
            {"rev": "bad0000000", "name": "broken", "data": null, "experiments": null,
             "error": {"type": "YAMLError", "msg": "bad params.yaml"}}
        ]);
        let table = parse(&json.to_string()).unwrap();
        assert_eq!(table.rows[1].error.as_deref(), Some("bad params.yaml"));
        assert!(table.rows[1].params.is_empty());
    }

    #[test]
    fn a_param_file_dvc_could_not_read_contributes_no_columns() {
        let json = json!([{"rev": "workspace", "name": null, "error": null, "experiments": null,
            "data": {"timestamp": null, "metrics": {}, "params": {"p.yaml": {"error": {"msg": "nope"}}}}}]);
        let table = parse(&json.to_string()).unwrap();
        assert!(table.param_columns.is_empty());
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse("not json").is_err());
        assert!(
            parse("{\"rev\": 1}").is_err(),
            "an object is not a list of revisions"
        );
    }

    // ---- 7.2 sorting ----

    fn numbers(values: &[(&str, Value)]) -> Table {
        let mut rows = vec![Row {
            kind: RowKind::Baseline,
            label: "base".into(),
            rev: "base".into(),
            timestamp: None,
            params: BTreeMap::new(),
            metrics: BTreeMap::new(),
            error: None,
            run_secs: None,
        }];
        for (name, value) in values {
            let mut params = BTreeMap::new();
            if !value.is_null() || *name == "nullish" {
                params.insert("p:v".to_owned(), value.clone());
            }
            rows.push(Row {
                kind: RowKind::Experiment,
                label: (*name).into(),
                rev: (*name).into(),
                timestamp: None,
                params,
                metrics: BTreeMap::new(),
                error: None,
                run_secs: None,
            });
        }
        Table {
            rows,
            param_columns: vec!["p:v".into()],
            metric_columns: vec![],
            messages: BTreeMap::new(),
            directions: BTreeMap::new(),
        }
    }

    fn sorted(table: &Table, direction: Direction) -> Vec<String> {
        let sort = Sort {
            by: Some((Column::Param("p:v".into()), direction)),
            ..Sort::default()
        };
        labels(&table.ordered(&sort))
    }

    #[test]
    fn numbers_sort_numerically_not_as_text() {
        let table = numbers(&[("ten", json!(10)), ("nine", json!(9)), ("two", json!(2.5))]);
        assert_eq!(
            sorted(&table, Direction::Ascending),
            ["base", "two", "nine", "ten"]
        );
        assert_eq!(
            sorted(&table, Direction::Descending),
            ["base", "ten", "nine", "two"]
        );
    }

    #[test]
    fn text_sorts_alphabetically_and_after_numbers() {
        let table = numbers(&[("b", json!("beta")), ("n", json!(1)), ("a", json!("alpha"))]);
        assert_eq!(
            sorted(&table, Direction::Ascending),
            ["base", "n", "a", "b"]
        );
    }

    #[test]
    fn rows_without_a_value_are_last_in_both_directions() {
        let table = numbers(&[("none", Value::Null), ("low", json!(1)), ("high", json!(5))]);
        assert_eq!(
            sorted(&table, Direction::Ascending),
            ["base", "low", "high", "none"]
        );
        assert_eq!(
            sorted(&table, Direction::Descending),
            ["base", "high", "low", "none"]
        );
    }

    #[test]
    fn explicit_null_is_a_value_unlike_an_absent_cell() {
        let table = numbers(&[
            ("nullish", Value::Null),
            ("absent", Value::Null.clone()),
            ("one", json!(1)),
        ]);
        // "nullish" has the JSON value null (text "null"); "absent" has no entry at all.
        let asc = sorted(&table, Direction::Ascending);
        assert_eq!(asc.last().unwrap(), "absent");
    }

    #[test]
    fn equal_values_keep_dvcs_order() {
        let table = numbers(&[
            ("first", json!(1)),
            ("second", json!(1)),
            ("third", json!(0)),
        ]);
        assert_eq!(
            sorted(&table, Direction::Ascending),
            ["base", "third", "first", "second"]
        );
        assert_eq!(
            sorted(&table, Direction::Descending),
            ["base", "first", "second", "third"]
        );
    }

    #[test]
    fn workspace_and_baselines_never_move() {
        let table = parse(&sample("two.json")).unwrap();
        let by_auc = |direction| Sort {
            by: Some((Column::Metric("metrics.json:auc".into()), direction)),
            ..Sort::default()
        };
        let asc = labels(&table.ordered(&by_auc(Direction::Ascending)));
        assert_eq!(asc, ["workspace", "master", "lr-005", "lr-010"]);
        let desc = labels(&table.ordered(&by_auc(Direction::Descending)));
        assert_eq!(desc, ["workspace", "master", "lr-010", "lr-005"]);
    }

    #[test]
    fn name_column_sorts_by_label() {
        let table = parse(&sample("two.json")).unwrap();
        let sort = Sort {
            by: Some((Column::Name, Direction::Ascending)),
            ..Sort::default()
        };
        assert_eq!(
            labels(&table.ordered(&sort)),
            ["workspace", "master", "lr-005", "lr-010"]
        );
    }

    #[test]
    fn no_sort_keeps_the_original_order() {
        let table = parse(&sample("two.json")).unwrap();
        assert_eq!(
            labels(&table.ordered(&Sort::default())),
            ["workspace", "master", "lr-010", "lr-005"]
        );
    }

    #[test]
    fn choosing_the_same_column_toggles_direction_and_a_new_one_starts_ascending() {
        let (a, b) = (Column::Name, Column::Param("p:v".into()));
        let mut sort = Sort::default();
        sort.choose(a.clone());
        assert_eq!(sort.direction_of(&a), Some(Direction::Ascending));
        sort.choose(a.clone());
        assert_eq!(sort.direction_of(&a), Some(Direction::Descending));
        sort.choose(a.clone());
        assert_eq!(sort.direction_of(&a), Some(Direction::Ascending));
        sort.choose(b.clone());
        assert_eq!(sort.direction_of(&b), Some(Direction::Ascending));
        assert_eq!(sort.direction_of(&a), None);
    }

    // ---- fetching ----

    fn fake_dvc(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-dvc");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn fetch_runs_exp_show_json_and_parses_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let json_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exp_show/two.json");
        let dvc = fake_dvc(
            dir.path(),
            &format!(
                "[ \"$*\" = \"exp show --json -A\" ] || {{ echo \"bad args: $*\" >&2; exit 9; }}\ncat '{}'",
                json_path.display()
            ),
        );
        let table = fetch(&dvc, dir.path()).unwrap();
        assert!(table.has_experiments());
    }

    #[test]
    fn fetch_reports_a_failing_command_with_its_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(
            dir.path(),
            "echo 'ERROR: not a git repository' >&2; exit 255",
        );
        let error = fetch(&dvc, dir.path()).unwrap_err();
        assert!(
            error.contains("exit 255") && error.contains("not a git repository"),
            "{error}"
        );
    }

    #[test]
    fn fetch_reports_output_that_is_not_json() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo 'surprise, plain text'");
        let error = fetch(&dvc, dir.path()).unwrap_err();
        assert!(error.contains("could not read the output"), "{error}");
    }

    #[test]
    fn fetch_reports_a_program_that_cannot_start() {
        let dir = tempfile::tempdir().unwrap();
        let error = fetch(&dir.path().join("missing"), dir.path()).unwrap_err();
        assert!(error.contains("cannot start"), "{error}");
    }

    // ---- differences from the baseline ----

    fn metric_row(kind: RowKind, label: &str, auc: serde_json::Value) -> Row {
        Row {
            kind,
            label: label.into(),
            rev: String::new(),
            timestamp: None,
            params: BTreeMap::from([("p:lr".to_owned(), json!(0.01))]),
            metrics: BTreeMap::from([("m:auc".to_owned(), auc)]),
            error: None,
            run_secs: None,
        }
    }

    #[test]
    fn baseline_of_is_the_nearest_baseline_above() {
        let rows = [
            metric_row(RowKind::Workspace, "workspace", json!(1)),
            metric_row(RowKind::Baseline, "main", json!(1)),
            metric_row(RowKind::Experiment, "a", json!(1)),
            metric_row(RowKind::Baseline, "other", json!(1)),
            metric_row(RowKind::Experiment, "b", json!(1)),
        ];
        let refs: Vec<&Row> = rows.iter().collect();
        let label = |i| baseline_of(&refs, i).map(|r| r.label.as_str());
        assert_eq!((label(0), label(1)), (None, None));
        assert_eq!((label(2), label(4)), (Some("main"), Some("other")));
        assert_eq!(label(9), None);
    }

    #[test]
    fn params_differ_only_for_experiments_against_their_baseline() {
        let base = metric_row(RowKind::Baseline, "main", json!(1));
        let mut exp = metric_row(RowKind::Experiment, "a", json!(1));
        let column = Column::Param("p:lr".into());
        assert!(!param_differs(&exp, Some(&base), &column));
        exp.params.insert("p:lr".into(), json!(0.05));
        assert!(param_differs(&exp, Some(&base), &column));
        assert!(!param_differs(
            &exp,
            Some(&base),
            &Column::Metric("m:auc".into())
        ));
        assert!(
            !param_differs(&base, None, &column),
            "baseline and workspace never differ"
        );
    }

    #[test]
    fn delta_uses_the_precision_of_the_operands_not_float_noise() {
        let base = metric_row(RowKind::Baseline, "main", json!(0.6633));
        let exp = metric_row(RowKind::Experiment, "a", json!(0.85));
        assert_eq!(
            metric_delta(&exp, &base, "m:auc"),
            Some((Change::Up, "+0.1867".into()))
        );
        let lower = metric_row(RowKind::Experiment, "b", json!(0.5));
        assert_eq!(
            metric_delta(&lower, &base, "m:auc"),
            Some((Change::Down, "-0.1633".into()))
        );
    }

    #[test]
    fn integer_metrics_give_integer_deltas_and_equal_values_no_change() {
        let base = metric_row(RowKind::Baseline, "main", json!(10));
        let exp = metric_row(RowKind::Experiment, "a", json!(13));
        assert_eq!(
            metric_delta(&exp, &base, "m:auc"),
            Some((Change::Up, "+3".into()))
        );
        let same = metric_row(RowKind::Experiment, "c", json!(10));
        assert_eq!(
            metric_delta(&same, &base, "m:auc"),
            Some((Change::Same, "±0".into()))
        );
    }

    #[test]
    fn missing_or_non_numeric_metrics_have_no_delta() {
        let base = metric_row(RowKind::Baseline, "main", json!(0.5));
        let text = metric_row(RowKind::Experiment, "a", json!("n/a"));
        assert_eq!(metric_delta(&text, &base, "m:auc"), None);
        let exp = metric_row(RowKind::Experiment, "b", json!(0.7));
        assert_eq!(metric_delta(&exp, &base, "m:other"), None);
        let base_text = metric_row(RowKind::Baseline, "main", json!(null));
        assert_eq!(metric_delta(&exp, &base_text, "m:auc"), None);
    }

    // ---- run time column ----

    #[test]
    fn the_run_time_column_sits_next_to_the_name_and_only_when_a_time_is_known() {
        let mut table = numbers(&[("a", json!(1)), ("b", json!(2))]);
        assert!(!table.columns().contains(&Column::Duration));
        table.rows[1].run_secs = Some(4.2);
        let columns = table.columns();
        assert_eq!(columns[1], Column::Duration);
        assert_eq!(Column::Duration.title(), "run time");
        assert_eq!(cell_text(&table.rows[1], &Column::Duration), "4.2s");
        assert_eq!(cell_text(&table.rows[2], &Column::Duration), "");
    }

    #[test]
    fn run_times_sort_numerically_with_unknown_ones_last() {
        let mut table = numbers(&[("slow", json!(1)), ("none", json!(2)), ("fast", json!(3))]);
        table.rows[1].run_secs = Some(120.0); // slow
        table.rows[3].run_secs = Some(9.0); // fast
        let mut sort = Sort::default();
        sort.choose(Column::Duration);
        let labels: Vec<&str> = table
            .ordered(&sort)
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["base", "fast", "slow", "none"],
            "9s before 2m, unknown last"
        );
        sort.choose(Column::Duration);
        let labels: Vec<&str> = table
            .ordered(&sort)
            .iter()
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["base", "slow", "fast", "none"],
            "descending keeps unknown last"
        );
    }
}
