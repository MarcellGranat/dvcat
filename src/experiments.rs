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
    /// Params that were hidden by hand (`v` / `H`).
    pub hidden: &'a BTreeSet<String>,
    /// Keep the columns whose value is the same in every row.
    pub show_unchanged: bool,
    /// Words that must all appear in a column's label (case does not matter).
    pub filter: &'a str,
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
            Column::Param(label) | Column::Metric(label) => Some(split_group(label).0),
            Column::Name | Column::Duration | Column::Age => None,
        }
    }

    /// The header text without the group, which is shown on a line of its own.
    pub fn short_title(&self) -> &str {
        match self {
            Column::Param(label) | Column::Metric(label) => split_group(label).1,
            other => other.title(),
        }
    }

    /// The header text.
    pub fn title(&self) -> &str {
        match self {
            Column::Name => "name",
            Column::Duration => "run time",
            Column::Age => "age",
            Column::Param(label) | Column::Metric(label) => label,
        }
    }
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
        std::iter::once(Column::Name)
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
            .chain(
                self.param_columns
                    .iter()
                    .filter(|label| params && !query.hidden.contains(*label) && wanted(label))
                    .cloned()
                    .map(Column::Param)
                    .filter(keep),
            )
            .chain(
                self.metric_columns
                    .iter()
                    .filter(|label| metrics && wanted(label))
                    .cloned()
                    .map(Column::Metric)
                    .filter(keep),
            )
            .collect()
    }

    pub fn has_experiments(&self) -> bool {
        self.rows.iter().any(|row| row.kind == RowKind::Experiment)
    }

    /// Rows in display order. Only experiments are reordered, and only within the run of
    /// experiments that belongs to one baseline; the workspace and baselines stay put.
    pub fn ordered(&self, sort: &Sort) -> Vec<&Row> {
        let Some((column, direction)) = &sort.by else {
            return self.rows.iter().collect();
        };
        let mut out: Vec<&Row> = Vec::with_capacity(self.rows.len());
        let mut run: Vec<&Row> = Vec::new();
        for row in &self.rows {
            if row.kind == RowKind::Experiment {
                run.push(row);
            } else {
                flush_run(&mut run, &mut out, column, *direction);
                out.push(row);
            }
        }
        flush_run(&mut run, &mut out, column, *direction);
        out
    }
}

/// Sort a run of experiments and move it to the end of `out`.
/// The named lifetime `'a` says both vectors hold references into the same table; a closure
/// cannot state that, which is why this is a function.
fn flush_run<'a>(
    run: &mut Vec<&'a Row>,
    out: &mut Vec<&'a Row>,
    column: &Column,
    direction: Direction,
) {
    // `sort_by` is stable: rows that compare equal keep the order DVC gave them.
    run.sort_by(|a, b| compare(sort_value(a, column), sort_value(b, column), direction));
    out.append(run);
}

/// The text shown in a cell; empty when the row has no such value.
pub fn cell_text(row: &Row, column: &Column) -> String {
    match column {
        Column::Name => row.label.clone(),
        Column::Param(label) => row.params.get(label).map(display).unwrap_or_default(),
        Column::Metric(label) => row.metrics.get(label).map(display).unwrap_or_default(),
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sort {
    pub by: Option<(Column, Direction)>,
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
        ColumnQuery {
            scope,
            hidden,
            show_unchanged,
            filter,
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
        }
    }

    fn sorted(table: &Table, direction: Direction) -> Vec<String> {
        let sort = Sort {
            by: Some((Column::Param("p:v".into()), direction)),
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
