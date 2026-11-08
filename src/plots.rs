//! DVC plots: the `plots:` section of `dvc.yaml` and the data files it points to.
//!
//! Pure functions, so the content of a plot is testable without drawing. Only the workspace is read;
//! comparing revisions (`dvc plots diff`) is not done here.

use std::{collections::BTreeMap, fs, path::Path};

use serde_json::Value as Json;
use serde_norway::Value as Yaml;

use crate::dvc_yaml::{self, scalar};

const DVC_YAML: &str = "dvc.yaml";
/// The implicit X field DVC uses when none is given: the row number.
pub const STEP: &str = "step";
const DEFAULT_TEMPLATE: &str = "linear";
/// Every template name `dvc plots` knows, for the form's hint.
pub const TEMPLATES: &str = "linear, simple, scatter, smooth, bar_horizontal, bar_horizontal_sorted, confusion, confusion_normalized";

/// How a plot is drawn, after DVC's built-in templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    /// `linear`, and `simple`.
    Line,
    /// `smooth` is drawn as a plain line: the terminal has no room for LOESS.
    Smooth,
    Scatter,
    /// `bar_horizontal` and `bar_horizontal_sorted`.
    Bar {
        sorted: bool,
    },
    /// `confusion` and `confusion_normalized`.
    Confusion {
        normalized: bool,
    },
}

impl Template {
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim_end_matches(".json") {
            "linear" | "simple" => Some(Self::Line),
            "smooth" => Some(Self::Smooth),
            "scatter" => Some(Self::Scatter),
            "bar_horizontal" => Some(Self::Bar { sorted: false }),
            "bar_horizontal_sorted" => Some(Self::Bar { sorted: true }),
            "confusion" => Some(Self::Confusion { normalized: false }),
            "confusion_normalized" => Some(Self::Confusion { normalized: true }),
            _ => None,
        }
    }
}

/// One line or one set of points.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub points: Vec<(f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Lines(Vec<Series>),
    /// Category and value, in the order to draw them.
    Bars(Vec<(String, f64)>),
    /// `counts[actual][predicted]`; `labels` name both axes.
    Matrix {
        labels: Vec<String>,
        counts: Vec<Vec<f64>>,
    },
    /// Nothing to draw, and why (missing file, unreadable data, an image).
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    pub id: String,
    pub template: Template,
    pub title: String,
    pub x_title: String,
    pub y_title: String,
    pub content: Content,
    /// The entry as it is written in `dvc.yaml`, under its `plots:` key.
    pub definition: String,
}

/// Where one plot's data comes from, as written in `dvc.yaml`.
#[derive(Debug, Clone, PartialEq, Default)]
struct Spec {
    id: String,
    /// File and the Y fields to take from it (empty: the file's last column).
    files: Vec<(String, Vec<String>)>,
    x: Option<String>,
    template: Option<String>,
    title: Option<String>,
    x_label: Option<String>,
    y_label: Option<String>,
    definition: String,
}

/// Load every plot of the project at `root`. A project without a `plots:` section has none.
pub fn load(root: &Path) -> Result<Vec<Plot>, String> {
    let path = root.join(DVC_YAML);
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {DVC_YAML}: {e}"))?;
    let specs = parse_specs(&text)?;
    Ok(specs.iter().map(|spec| build(root, spec)).collect())
}

fn parse_specs(text: &str) -> Result<Vec<Spec>, String> {
    let doc: Yaml =
        serde_norway::from_str(text).map_err(|e| format!("cannot parse {DVC_YAML}: {e}"))?;
    let Some(entries) = doc.get("plots") else {
        return Ok(Vec::new());
    };
    // `plots:` is a list; a mapping of id to definition is accepted too.
    let entries: Vec<Yaml> = match entries {
        Yaml::Sequence(list) => list.clone(),
        Yaml::Mapping(map) => map
            .iter()
            .map(|(k, v)| {
                let mut single = serde_norway::Mapping::new();
                single.insert(k.clone(), v.clone());
                Yaml::Mapping(single)
            })
            .collect(),
        Yaml::Null => Vec::new(),
        _ => return Err("'plots' in dvc.yaml must be a list".to_owned()),
    };
    entries
        .iter()
        .map(|entry| {
            let mut spec = parse_spec(entry)?;
            spec.definition = definition_text(entry);
            Ok(spec)
        })
        .collect()
}

fn definition_text(entry: &Yaml) -> String {
    let mut wrapper = serde_norway::Mapping::new();
    wrapper.insert(
        Yaml::String("plots".to_owned()),
        Yaml::Sequence(vec![entry.clone()]),
    );
    serde_norway::to_string(&Yaml::Mapping(wrapper)).unwrap_or_default()
}

/// A field name or a list of them.
fn names(value: &Yaml) -> Vec<String> {
    match value {
        Yaml::String(s) => vec![s.clone()],
        Yaml::Sequence(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_spec(entry: &Yaml) -> Result<Spec, String> {
    if let Some(path) = entry.as_str() {
        return Ok(Spec {
            id: path.to_owned(),
            files: vec![(path.to_owned(), Vec::new())],
            ..Spec::default()
        });
    }
    let map = entry.as_mapping().filter(|m| m.len() == 1);
    let Some((key, config)) = map.and_then(|m| m.iter().next()) else {
        return Err("a plot in dvc.yaml is a path or a single `id: definition`".to_owned());
    };
    let key = key.as_str().ok_or("a plot id must be text")?.to_owned();
    let get_text = |field: &str| config.get(field).and_then(Yaml::as_str).map(str::to_owned);

    let mut spec = Spec {
        id: key.clone(),
        template: get_text("template"),
        title: get_text("title"),
        x_label: get_text("x_label"),
        y_label: get_text("y_label"),
        ..Spec::default()
    };
    let y = config.get("y");
    let x = config.get("x");
    // Per-file fields (`y: {file: [a, b]}`) mean the key is only an id; otherwise it is the file.
    if let Some(Yaml::Mapping(files)) = y {
        for (file, fields) in files {
            if let Some(file) = file.as_str() {
                spec.files.push((file.to_owned(), names(fields)));
            }
        }
        spec.x = x.and_then(|x| names(x).into_iter().next());
    } else if let Some(Yaml::Mapping(files)) = x {
        let fields = y.map(names).unwrap_or_default();
        for (file, field) in files {
            if let Some(file) = file.as_str() {
                spec.files.push((file.to_owned(), fields.clone()));
                spec.x = spec.x.take().or_else(|| names(field).into_iter().next());
            }
        }
    } else {
        spec.files.push((key, y.map(names).unwrap_or_default()));
        spec.x = x.and_then(|x| names(x).into_iter().next());
    }
    Ok(spec)
}

// ---------------------------------------------------------------------------------------------
// Data files
// ---------------------------------------------------------------------------------------------

/// A data file as a table of text cells.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Records {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Records {
    fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    fn numbers(&self, column: usize) -> Vec<Option<f64>> {
        self.rows
            .iter()
            .map(|row| {
                row.get(column)
                    .and_then(|cell| cell.trim().parse::<f64>().ok())
            })
            .map(|n| n.filter(|n| n.is_finite()))
            .collect()
    }
}

fn read_records(root: &Path, file: &str) -> Result<Records, String> {
    let extension = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg") {
        return Err(format!("{file} is an image; a terminal cannot show it"));
    }
    let text =
        fs::read_to_string(root.join(file)).map_err(|e| format!("cannot read {file}: {e}"))?;
    match extension.as_str() {
        "csv" => Ok(parse_delimited(&text, ',')),
        "tsv" => Ok(parse_delimited(&text, '\t')),
        "json" => {
            let value = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
            records_from_json(&value).ok_or_else(|| format!("{file}: expected a list of objects"))
        }
        "yaml" | "yml" => {
            let value: Json = serde_norway::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
            records_from_json(&value).ok_or_else(|| format!("{file}: expected a list of objects"))
        }
        _ => Err(format!("{file}: DVC plots read csv, tsv, json and yaml")),
    }
}

/// CSV or TSV with a header line. Quoted cells may hold the delimiter and doubled quotes.
pub fn parse_delimited(text: &str, delimiter: char) -> Records {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else {
        return Records::default();
    };
    Records {
        columns: split_line(header, delimiter),
        rows: lines.map(|line| split_line(line, delimiter)).collect(),
    }
}

fn split_line(line: &str, delimiter: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.trim_end_matches('\r').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delimiter && !quoted => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells
}

/// An array of objects, or an object holding one (DVCLive writes `{"train": [...]}`).
fn records_from_json(value: &Json) -> Option<Records> {
    let list = match value {
        Json::Array(list) => list,
        Json::Object(map) => map.values().find_map(Json::as_array)?,
        _ => return None,
    };
    let mut columns: Vec<String> = Vec::new();
    for item in list {
        for key in item.as_object()?.keys() {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    let rows = list
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|c| match item.get(c) {
                    Some(Json::String(s)) => s.clone(),
                    Some(Json::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                })
                .collect()
        })
        .collect();
    Some(Records { columns, rows })
}

// ---------------------------------------------------------------------------------------------
// Building a plot
// ---------------------------------------------------------------------------------------------

fn build(root: &Path, spec: &Spec) -> Plot {
    let template_name = spec.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);
    let template = Template::parse(template_name);
    let mut plot = Plot {
        id: spec.id.clone(),
        template: template.unwrap_or(Template::Line),
        title: spec.title.clone().unwrap_or_else(|| spec.id.clone()),
        x_title: spec
            .x_label
            .clone()
            .or_else(|| spec.x.clone())
            .unwrap_or_else(|| STEP.to_owned()),
        y_title: String::new(),
        content: Content::Unavailable(String::new()),
        definition: spec.definition.clone(),
    };
    if template.is_none() {
        plot.content = Content::Unavailable(format!("unknown template '{template_name}'"));
        return plot;
    }

    let mut loaded: Vec<(&str, Records, &[String])> = Vec::new();
    for (file, fields) in &spec.files {
        match read_records(root, file) {
            Ok(records) => loaded.push((file, records, fields)),
            Err(reason) => {
                plot.content = Content::Unavailable(reason);
                return plot;
            }
        }
    }
    let result = match plot.template {
        Template::Bar { sorted } => bars(&loaded, spec, sorted, &mut plot),
        Template::Confusion { normalized } => matrix(&loaded, spec, normalized, &mut plot),
        _ => lines(&loaded, spec, &mut plot),
    };
    plot.content = result.unwrap_or_else(Content::Unavailable);
    plot
}

fn lines(
    loaded: &[(&str, Records, &[String])],
    spec: &Spec,
    plot: &mut Plot,
) -> Result<Content, String> {
    let many_files = loaded.len() > 1;
    let mut series = Vec::new();
    let mut y_names: Vec<String> = Vec::new();
    for (file, records, fields) in loaded {
        let ys: Vec<String> = if fields.is_empty() {
            vec![
                default_y(records, spec.x.as_deref())
                    .ok_or_else(|| format!("{file} has no columns"))?,
            ]
        } else {
            fields.to_vec()
        };
        let x_values = x_values(records, spec.x.as_deref()).map_err(|m| format!("{file}: {m}"))?;
        for y in ys {
            let column = records
                .column(&y)
                .ok_or_else(|| format!("{file} has no column '{y}'"))?;
            let points: Vec<(f64, f64)> = x_values
                .iter()
                .zip(records.numbers(column))
                .filter_map(|(&x, y)| Some((x?, y?)))
                .collect();
            if !y_names.contains(&y) {
                y_names.push(y.clone());
            }
            let name = if many_files {
                let stem = Path::new(file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(file);
                format!("{stem}:{y}")
            } else {
                y
            };
            series.push(Series { name, points });
        }
    }
    plot.y_title = plot_label(spec.y_label.as_deref(), &y_names);
    if series.iter().all(|s| s.points.is_empty()) {
        return Err("no numeric values to plot".to_owned());
    }
    Ok(Content::Lines(series))
}

fn plot_label(label: Option<&str>, names: &[String]) -> String {
    label.map(str::to_owned).unwrap_or_else(|| names.join(", "))
}

/// The explicit X field, a `step` column, or else the row number (starting at 0, as DVC does).
fn x_values(records: &Records, x: Option<&str>) -> Result<Vec<Option<f64>>, String> {
    let field = x.unwrap_or(STEP);
    match records.column(field) {
        Some(column) => Ok(records.numbers(column)),
        None if x.is_none() || field == STEP => {
            Ok((0..records.rows.len()).map(|i| Some(i as f64)).collect())
        }
        None => Err(format!("no column '{field}'")),
    }
}

/// The last column that is not the X field.
fn default_y(records: &Records, x: Option<&str>) -> Option<String> {
    let x = x.unwrap_or(STEP);
    records.columns.iter().rev().find(|c| *c != x).cloned()
}

/// DVC's bar templates put the number on X and the category on Y.
fn bars(
    loaded: &[(&str, Records, &[String])],
    spec: &Spec,
    sorted: bool,
    plot: &mut Plot,
) -> Result<Content, String> {
    let (file, records, fields) = loaded.first().ok_or("no data file")?;
    let value_field = spec
        .x
        .clone()
        .or_else(|| records.columns.first().cloned())
        .ok_or_else(|| format!("{file} has no columns"))?;
    let label_field = fields
        .first()
        .cloned()
        .or_else(|| records.columns.iter().find(|c| **c != value_field).cloned())
        .ok_or_else(|| format!("{file} needs a second column for the bar labels"))?;
    let value_column = records
        .column(&value_field)
        .ok_or_else(|| format!("{file} has no column '{value_field}'"))?;
    let label_column = records
        .column(&label_field)
        .ok_or_else(|| format!("{file} has no column '{label_field}'"))?;
    plot.x_title = spec.x_label.clone().unwrap_or(value_field);
    plot.y_title = spec.y_label.clone().unwrap_or(label_field);
    let mut bars: Vec<(String, f64)> = records
        .numbers(value_column)
        .into_iter()
        .zip(&records.rows)
        .filter_map(|(value, row)| Some((row.get(label_column)?.clone(), value?)))
        .collect();
    if bars.is_empty() {
        return Err("no numeric values to plot".to_owned());
    }
    if sorted {
        bars.sort_by(|a, b| b.1.total_cmp(&a.1));
    }
    Ok(Content::Bars(bars))
}

/// Count `(actual, predicted)` pairs. X is the actual class, Y the predicted one.
fn matrix(
    loaded: &[(&str, Records, &[String])],
    spec: &Spec,
    normalized: bool,
    plot: &mut Plot,
) -> Result<Content, String> {
    let (file, records, fields) = loaded.first().ok_or("no data file")?;
    let actual_field = spec
        .x
        .clone()
        .or_else(|| records.columns.first().cloned())
        .ok_or("no columns")?;
    let predicted_field = fields
        .first()
        .cloned()
        .or_else(|| {
            records
                .columns
                .iter()
                .find(|c| **c != actual_field)
                .cloned()
        })
        .ok_or_else(|| format!("{file} needs actual and predicted columns"))?;
    let actual = records
        .column(&actual_field)
        .ok_or_else(|| format!("{file} has no column '{actual_field}'"))?;
    let predicted = records
        .column(&predicted_field)
        .ok_or_else(|| format!("{file} has no column '{predicted_field}'"))?;
    plot.x_title = actual_field;
    plot.y_title = predicted_field;

    let mut labels: Vec<String> = Vec::new();
    let mut seen: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    let index_of =
        |label: &str, labels: &mut Vec<String>| match labels.iter().position(|l| l == label) {
            Some(i) => i,
            None => {
                labels.push(label.to_owned());
                labels.len() - 1
            }
        };
    for row in &records.rows {
        let (Some(a), Some(p)) = (row.get(actual), row.get(predicted)) else {
            continue;
        };
        let key = (index_of(a, &mut labels), index_of(p, &mut labels));
        *seen.entry(key).or_default() += 1.0;
    }
    if labels.is_empty() {
        return Err("no rows to count".to_owned());
    }
    // Numbers sort numerically ("2" before "10"), the rest as text.
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by(
        |&a, &b| match (labels[a].parse::<f64>(), labels[b].parse::<f64>()) {
            (Ok(x), Ok(y)) => x.total_cmp(&y),
            _ => labels[a].cmp(&labels[b]),
        },
    );
    let n = labels.len();
    let mut counts = vec![vec![0.0; n]; n];
    for (&(a, p), &count) in &seen {
        let (row, col) = (
            order.iter().position(|&i| i == a),
            order.iter().position(|&i| i == p),
        );
        if let (Some(row), Some(col)) = (row, col) {
            counts[row][col] = count;
        }
    }
    if normalized {
        for row in &mut counts {
            let total: f64 = row.iter().sum();
            if total > 0.0 {
                row.iter_mut().for_each(|c| *c /= total);
            }
        }
    }
    let labels = order.iter().map(|&i| labels[i].clone()).collect();
    Ok(Content::Matrix { labels, counts })
}

// ---------------------------------------------------------------------------------------------
// Adding a plot
// ---------------------------------------------------------------------------------------------

/// What the "new plot" form collects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    pub file: String,
    pub x: String,
    pub y: Vec<String>,
    pub template: String,
    pub title: String,
}

impl Draft {
    /// The list item for `dvc.yaml`, its dash indented by `indent` spaces. Fields left empty are
    /// not written, so DVC's defaults apply.
    pub fn entry(&self, indent: usize) -> String {
        let pad = " ".repeat(indent);
        let file = scalar(&self.file);
        let mut fields: Vec<String> = Vec::new();
        if !self.x.is_empty() {
            fields.push(format!("x: {}", scalar(&self.x)));
        }
        match self.y.as_slice() {
            [] => {}
            [one] => fields.push(format!("y: {}", scalar(one))),
            many => fields.push(format!(
                "y: [{}]",
                many.iter()
                    .map(|y| scalar(y))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
        if !self.template.is_empty() && self.template != DEFAULT_TEMPLATE {
            fields.push(format!("template: {}", scalar(&self.template)));
        }
        if !self.title.is_empty() {
            fields.push(format!("title: {}", scalar(&self.title)));
        }
        if fields.is_empty() {
            return format!("{pad}- {file}\n");
        }
        let mut text = format!("{pad}- {file}:\n");
        for field in fields {
            text.push_str(&format!("{pad}    {field}\n"));
        }
        text
    }
}

/// `dvc.yaml` with the plot added to its `plots:` section (created at the end when missing).
/// The rest of the file, comments included, is left as it was.
pub fn add_to_dvc_yaml(text: &str, draft: &Draft) -> Result<String, String> {
    dvc_yaml::insert_into_section(
        text,
        "plots",
        |indent| draft.entry(indent),
        |line| line.trim_start().starts_with("- "),
    )
}

const DATA_EXTENSIONS: [&str; 5] = ["csv", "tsv", "json", "yaml", "yml"];

/// Data files a plot can read, as paths relative to `root`, in directory order. Hidden
/// directories (`.git`, `.dvc`, `.venv`) and `dvc.yaml` are left out.
pub fn iter_data_files(root: &Path) -> impl Iterator<Item = String> {
    dvc_yaml::iter_files(root, |name, path| {
        name != DVC_YAML
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .is_some_and(|e| DATA_EXTENSIONS.contains(&e.as_str()))
    })
}

/// The column names of a data file, for the form's hint.
pub fn columns(root: &Path, file: &str) -> Result<Vec<String>, String> {
    read_records(root, file).map(|r| r.columns)
}

impl Plot {
    /// Ranges `(x_min, x_max, y_min, y_max)` of a line plot, padded so lines never sit on the frame.
    pub fn bounds(series: &[Series]) -> (f64, f64, f64, f64) {
        let range = |values: &mut dyn Iterator<Item = f64>| {
            let (low, high) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| {
                (low.min(v), high.max(v))
            });
            if !low.is_finite() {
                return (0.0, 1.0);
            }
            let pad = if high > low {
                (high - low) * 0.05
            } else {
                low.abs().max(1.0) * 0.1
            };
            (low - pad, high + pad)
        };
        let (x_min, x_max) = range(&mut series.iter().flat_map(|s| s.points.iter().map(|p| p.0)));
        let (y_min, y_max) = range(&mut series.iter().flat_map(|s| s.points.iter().map(|p| p.1)));
        (x_min, x_max, y_min, y_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project(yaml: &str, files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(DVC_YAML), yaml).unwrap();
        for (name, text) in files {
            fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn lines_of(plot: &Plot) -> &[Series] {
        match &plot.content {
            Content::Lines(series) => series,
            other => panic!("not lines: {other:?}"),
        }
    }

    #[test]
    fn no_plots_section_means_no_plots() {
        let dir = project("stages: {}\n", &[]);
        assert!(load(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_plain_path_plots_the_last_column_against_the_row_number() {
        let dir = project(
            "plots:\n  - acc.csv\n",
            &[(
                "acc.csv",
                "epoch,loss,acc\n1,0.9,0.5\n2,0.5,0.7\n3,0.4,0.8\n",
            )],
        );
        let plots = load(dir.path()).unwrap();
        assert_eq!(plots.len(), 1);
        assert_eq!(plots[0].id, "acc.csv");
        assert_eq!(
            lines_of(&plots[0]),
            [Series {
                name: "acc".into(),
                points: vec![(0.0, 0.5), (1.0, 0.7), (2.0, 0.8)]
            }]
        );
    }

    #[test]
    fn x_and_y_fields_and_labels_come_from_the_definition() {
        let yaml = "plots:\n  - acc.csv:\n      x: epoch\n      y: [loss, acc]\n      title: Training\n      y_label: value\n";
        let dir = project(
            yaml,
            &[("acc.csv", "epoch,loss,acc\n1,0.9,0.5\n2,0.5,0.7\n")],
        );
        let plot = &load(dir.path()).unwrap()[0];
        assert_eq!(
            (
                plot.title.as_str(),
                plot.x_title.as_str(),
                plot.y_title.as_str()
            ),
            ("Training", "epoch", "value")
        );
        let series = lines_of(plot);
        assert_eq!(series[0].points, [(1.0, 0.9), (2.0, 0.5)]);
        assert_eq!(series[1].name, "acc");
    }

    #[test]
    fn several_files_under_one_id_become_one_series_each() {
        let yaml = "plots:\n  - compare:\n      x: step\n      y:\n        a.json: score\n        b.json: score\n";
        let dir = project(
            yaml,
            &[
                ("a.json", r#"[{"score": 1}, {"score": 2}]"#),
                ("b.json", r#"{"train": [{"score": 5}]}"#),
            ],
        );
        let plot = &load(dir.path()).unwrap()[0];
        assert_eq!(plot.id, "compare");
        let names: Vec<_> = lines_of(plot).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a:score", "b:score"]);
    }

    #[test]
    fn a_missing_file_or_column_is_explained_not_fatal() {
        let yaml = "plots:\n  - gone.csv\n  - acc.csv:\n      y: nope\n";
        let dir = project(yaml, &[("acc.csv", "a,b\n1,2\n")]);
        let plots = load(dir.path()).unwrap();
        assert!(matches!(&plots[0].content, Content::Unavailable(m) if m.contains("gone.csv")));
        assert!(matches!(&plots[1].content, Content::Unavailable(m) if m.contains("'nope'")));
    }

    #[test]
    fn images_and_unknown_templates_are_reported() {
        let yaml = "plots:\n  - roc.png\n  - acc.csv:\n      template: fancy\n";
        let dir = project(yaml, &[("acc.csv", "a,b\n1,2\n")]);
        let plots = load(dir.path()).unwrap();
        assert!(matches!(&plots[0].content, Content::Unavailable(m) if m.contains("image")));
        assert!(matches!(&plots[1].content, Content::Unavailable(m) if m.contains("fancy")));
    }

    #[test]
    fn bar_plots_take_the_number_from_x_and_sort_on_request() {
        let yaml = "plots:\n  - f.csv:\n      template: bar_horizontal_sorted\n      x: score\n      y: model\n";
        let dir = project(yaml, &[("f.csv", "model,score\nsvm,0.7\nrf,0.9\nlr,0.5\n")]);
        let plot = &load(dir.path()).unwrap()[0];
        assert_eq!(
            plot.content,
            Content::Bars(vec![
                ("rf".into(), 0.9),
                ("svm".into(), 0.7),
                ("lr".into(), 0.5)
            ])
        );
    }

    #[test]
    fn confusion_counts_actual_against_predicted() {
        let yaml =
            "plots:\n  - c.csv:\n      template: confusion\n      x: actual\n      y: predicted\n";
        let dir = project(
            yaml,
            &[("c.csv", "actual,predicted\n0,0\n0,1\n1,1\n1,1\n10,10\n")],
        );
        let plot = &load(dir.path()).unwrap()[0];
        let Content::Matrix { labels, counts } = &plot.content else {
            panic!("{:?}", plot.content)
        };
        assert_eq!(labels, &["0", "1", "10"]);
        assert_eq!(
            counts,
            &vec![
                vec![1.0, 1.0, 0.0],
                vec![0.0, 2.0, 0.0],
                vec![0.0, 0.0, 1.0]
            ]
        );
    }

    #[test]
    fn quoted_cells_may_hold_the_delimiter() {
        let records = parse_delimited("a,b\n\"x,y\",\"say \"\"hi\"\"\"\n", ',');
        assert_eq!(records.rows, [["x,y", "say \"hi\""]]);
    }

    #[test]
    fn a_plot_remembers_its_definition_as_written_in_dvc_yaml() {
        let dir = project(
            "plots:\n  - acc.csv:\n      x: epoch\n",
            &[("acc.csv", "epoch,acc\n1,2\n")],
        );
        let plot = &load(dir.path()).unwrap()[0];
        assert!(
            plot.definition.starts_with("plots:\n"),
            "{}",
            plot.definition
        );
        assert!(
            plot.definition.contains("acc.csv:") && plot.definition.contains("x: epoch"),
            "{}",
            plot.definition
        );
    }

    fn draft(file: &str) -> Draft {
        Draft {
            file: file.into(),
            ..Draft::default()
        }
    }

    #[test]
    fn a_bare_draft_is_a_plain_path_and_options_make_a_mapping() {
        assert_eq!(draft("a.csv").entry(2), "  - a.csv\n");
        let full = Draft {
            file: "a.csv".into(),
            x: "epoch".into(),
            y: vec!["loss".into(), "val loss".into()],
            template: "scatter".into(),
            title: "Run: 1".into(),
        };
        assert_eq!(
            full.entry(2),
            "  - a.csv:\n      x: epoch\n      y: [loss, val loss]\n      template: scatter\n      title: \"Run: 1\"\n"
        );
    }

    #[test]
    fn adding_creates_the_section_when_it_is_missing() {
        let out = add_to_dvc_yaml("stages: {}", &draft("a.csv")).unwrap();
        assert_eq!(out, "stages: {}\nplots:\n  - a.csv\n");
    }

    #[test]
    fn adding_appends_to_the_section_and_leaves_the_rest_alone() {
        let text = "plots:\n  # curves\n  - a.csv:\n      x: step\n\nstages:\n  s:\n    cmd: x\n";
        let out = add_to_dvc_yaml(text, &draft("b.csv")).unwrap();
        assert_eq!(
            out,
            "plots:\n  # curves\n  - a.csv:\n      x: step\n  - b.csv\n\nstages:\n  s:\n    cmd: x\n"
        );
        // Still valid, and the new plot is listed after the old one.
        let specs = parse_specs(&out).unwrap();
        assert_eq!(
            specs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["a.csv", "b.csv"]
        );
    }

    #[test]
    fn adding_follows_the_indent_of_a_section_written_without_one() {
        let out = add_to_dvc_yaml("plots:\n- a.csv\n", &draft("b.csv")).unwrap();
        assert_eq!(out, "plots:\n- a.csv\n- b.csv\n");
    }

    #[test]
    fn an_inline_section_is_refused_not_mangled() {
        assert!(add_to_dvc_yaml("plots: []\n", &draft("b.csv")).is_err());
    }

    #[test]
    fn data_files_are_found_below_the_root_without_hidden_or_build_directories() {
        let dir = project(
            "plots: []\n",
            &[("a.csv", ""), ("notes.txt", ""), ("params.yaml", "")],
        );
        for sub in ["metrics", ".git", "target", "metrics/deep"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        for file in [
            "metrics/b.json",
            "metrics/deep/c.TSV",
            ".git/x.csv",
            "target/y.csv",
        ] {
            fs::write(dir.path().join(file), "").unwrap();
        }
        let found: Vec<String> = iter_data_files(dir.path()).collect();
        assert_eq!(
            found,
            [
                "a.csv",
                "metrics/b.json",
                "metrics/deep/c.TSV",
                "params.yaml"
            ]
        );
    }

    #[test]
    fn bounds_pad_the_ranges() {
        let series = [Series {
            name: "s".into(),
            points: vec![(0.0, 1.0), (10.0, 1.0)],
        }];
        let (x0, x1, y0, y1) = Plot::bounds(&series);
        assert!(x0 < 0.0 && x1 > 10.0 && y0 < 1.0 && y1 > 1.0);
    }

    #[test]
    fn the_demo_project_has_a_training_curve_and_a_scatter() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo");
        let plots = load(&root).unwrap();
        let ids: Vec<_> = plots.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["training", "data/features.csv"]);
        assert_eq!(lines_of(&plots[0]).len(), 2);
        assert_eq!(plots[1].template, Template::Scatter);
        assert!(plots.iter().all(|p| matches!(p.content, Content::Lines(_))));
    }
}
