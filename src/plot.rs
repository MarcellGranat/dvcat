//! The numbers behind the plot panel: one point per experiment, a metric on Y.
//!
//! Pure functions over the experiments table, so the chart's content is testable without drawing.

use crate::experiments::{Column, Row, RowKind, Sort, Table};

/// One experiment on the chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    /// The experiment's position in the table's display order (`Table::ordered`), to match the cursor.
    pub row: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    pub points: Vec<Point>,
    /// Name of the X axis: the param column, or "creation order".
    pub x_title: String,
    pub y_title: String,
    /// Whether X is the experiments' creation order (a fallback when the cursor is not on a numeric param).
    pub by_order: bool,
}

/// Metric columns that can be plotted: numeric for at least two experiments.
pub fn plottable_metrics(table: &Table) -> Vec<String> {
    table
        .metric_columns
        .iter()
        .filter(|label| {
            table
                .rows
                .iter()
                .filter(|row| row.kind == RowKind::Experiment)
                .filter(|row| numeric(row.metrics.get(*label)).is_some())
                .count()
                >= 2
        })
        .cloned()
        .collect()
}

fn numeric(value: Option<&serde_json::Value>) -> Option<f64> {
    value?.as_f64().filter(|number| number.is_finite())
}

/// Points for `metric` on Y. X is the numeric param under `x_column`; for any other column (or a
/// param that is not numeric for at least two experiments) it is the creation order, 1 for the oldest.
pub fn plot(table: &Table, sort: &Sort, x_column: Option<&Column>, metric: &str) -> Plot {
    let displayed = table.ordered(sort);
    // (display position, row) of every experiment that has a numeric value for the metric.
    let with_metric: Vec<(usize, &Row, f64)> = displayed
        .iter()
        .enumerate()
        .filter(|(_, row)| row.kind == RowKind::Experiment)
        .filter_map(|(position, row)| numeric(row.metrics.get(metric)).map(|y| (position, *row, y)))
        .collect();

    let param_label = match x_column {
        Some(Column::Param(label)) => Some(label),
        _ => None,
    };
    if let Some(label) = param_label {
        let points: Vec<Point> = with_metric
            .iter()
            .filter_map(|&(row_index, row, y)| {
                numeric(row.params.get(label)).map(|x| Point {
                    x,
                    y,
                    row: row_index,
                    label: row.label.clone(),
                })
            })
            .collect();
        if points.len() >= 2 {
            return Plot {
                points,
                x_title: label.clone(),
                y_title: metric.to_owned(),
                by_order: false,
            };
        }
    }

    // Creation order: oldest first; experiments without a timestamp keep their table order, last.
    let mut ordered = with_metric;
    ordered.sort_by(|a, b| match (&a.1.timestamp, &b.1.timestamp) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let points = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (row_index, row, y))| Point {
            x: (index + 1) as f64,
            y,
            row: row_index,
            label: row.label.clone(),
        })
        .collect();
    Plot {
        points,
        x_title: "creation order".to_owned(),
        y_title: metric.to_owned(),
        by_order: true,
    }
}

impl Plot {
    /// Value ranges `(x_min, x_max, y_min, y_max)`, padded so points never sit on the frame and
    /// never degenerate to a zero-width range.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let range = |values: &mut dyn Iterator<Item = f64>| {
            let (low, high) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| {
                (low.min(v), high.max(v))
            });
            if !low.is_finite() {
                return (0.0, 1.0);
            }
            let pad = if high > low {
                (high - low) * 0.08
            } else {
                low.abs().max(1.0) * 0.1
            };
            (low - pad, high + pad)
        };
        let (x_min, x_max) = range(&mut self.points.iter().map(|p| p.x));
        let (y_min, y_max) = range(&mut self.points.iter().map(|p| p.y));
        (x_min, x_max, y_min, y_max)
    }

    /// The point belonging to the table row `row` (display position), if it is plotted.
    pub fn point_for_row(&self, row: usize) -> Option<&Point> {
        self.points.iter().find(|point| point.row == row)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use serde_json::json;

    use super::*;
    use crate::experiments;

    fn two() -> Table {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        experiments::parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn exp(label: &str, time: Option<&str>, lr: serde_json::Value, auc: serde_json::Value) -> Row {
        Row {
            kind: RowKind::Experiment,
            label: label.into(),
            rev: String::new(),
            timestamp: time.map(str::to_owned),
            params: BTreeMap::from([("p:lr".to_owned(), lr)]),
            metrics: BTreeMap::from([("m:auc".to_owned(), auc)]),
            error: None,
            run_secs: None,
        }
    }

    fn table(rows: Vec<Row>) -> Table {
        Table {
            rows,
            param_columns: vec!["p:lr".into()],
            metric_columns: vec!["m:auc".into()],
        }
    }

    #[test]
    fn param_on_x_gives_one_point_per_experiment() {
        let table = two();
        let metric = plottable_metrics(&table)
            .into_iter()
            .find(|m| m.ends_with("auc"))
            .unwrap();
        let lr = Column::Param("params.yaml:train.lr".into());
        let plot = plot(&table, &Sort::default(), Some(&lr), &metric);
        let mut points: Vec<(f64, &str)> = plot
            .points
            .iter()
            .map(|p| (p.x, p.label.as_str()))
            .collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(
            points.len(),
            2,
            "the two experiments, not the baseline or workspace"
        );
        assert_eq!(points[0], (0.05, "lr-005"));
        assert_eq!(points[1], (0.1, "lr-010"));
        assert!(!plot.by_order);
        assert_eq!(plot.x_title, "params.yaml:train.lr");
    }

    #[test]
    fn other_columns_plot_the_creation_order() {
        let table = table(vec![
            exp("new", Some("2024-02-01"), json!(0.1), json!(0.9)),
            exp("old", Some("2024-01-01"), json!(0.2), json!(0.5)),
            exp("undated", None, json!(0.3), json!(0.7)),
        ]);
        let plot = plot(&table, &Sort::default(), Some(&Column::Name), "m:auc");
        let order: Vec<(f64, &str)> = plot
            .points
            .iter()
            .map(|p| (p.x, p.label.as_str()))
            .collect();
        assert_eq!(order, [(1.0, "old"), (2.0, "new"), (3.0, "undated")]);
        assert!(plot.by_order);
        assert_eq!(plot.x_title, "creation order");
    }

    #[test]
    fn a_non_numeric_param_falls_back_to_creation_order() {
        let table = table(vec![
            exp("a", Some("1"), json!("fast"), json!(0.5)),
            exp("b", Some("2"), json!("slow"), json!(0.6)),
        ]);
        let plot = plot(
            &table,
            &Sort::default(),
            Some(&Column::Param("p:lr".into())),
            "m:auc",
        );
        assert!(plot.by_order);
        assert_eq!(plot.points.len(), 2);
    }

    #[test]
    fn missing_and_non_numeric_metrics_are_skipped() {
        let table = table(vec![
            exp("a", Some("1"), json!(0.1), json!(0.5)),
            exp("b", Some("2"), json!(0.2), json!(null)),
            exp("c", Some("3"), json!(0.3), json!("n/a")),
            exp("d", Some("4"), json!(0.4), json!(0.9)),
        ]);
        let plot = plot(&table, &Sort::default(), None, "m:auc");
        let labels: Vec<&str> = plot.points.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["a", "d"]);
    }

    #[test]
    fn plottable_metrics_need_two_numeric_experiments() {
        let one = table(vec![exp("a", None, json!(0.1), json!(0.5))]);
        assert!(plottable_metrics(&one).is_empty());
        let two_rows = table(vec![
            exp("a", None, json!(0.1), json!(0.5)),
            exp("b", None, json!(0.2), json!(0.6)),
        ]);
        assert_eq!(plottable_metrics(&two_rows), ["m:auc"]);
    }

    #[test]
    fn points_remember_their_display_row_and_follow_the_sort() {
        let table = two();
        let metric = plottable_metrics(&table)
            .into_iter()
            .find(|m| m.ends_with("auc"))
            .unwrap();
        let mut sort = Sort::default();
        sort.choose(Column::Metric(metric.clone()));
        let plot = plot(&table, &sort, None, &metric);
        let displayed = table.ordered(&sort);
        for point in &plot.points {
            assert_eq!(displayed[point.row].label, point.label);
        }
        assert!(
            plot.point_for_row(0).is_none(),
            "the workspace row has no point"
        );
    }

    #[test]
    fn bounds_are_padded_and_never_empty() {
        let table = table(vec![
            exp("a", Some("1"), json!(1), json!(0.5)),
            exp("b", Some("2"), json!(1), json!(0.5)),
        ]);
        let plot = plot(
            &table,
            &Sort::default(),
            Some(&Column::Param("p:lr".into())),
            "m:auc",
        );
        let (x0, x1, y0, y1) = plot.bounds();
        assert!(
            x0 < 1.0 && x1 > 1.0 && y0 < 0.5 && y1 > 0.5,
            "{:?}",
            (x0, x1, y0, y1)
        );
        let empty = Plot {
            points: vec![],
            x_title: String::new(),
            y_title: String::new(),
            by_order: true,
        };
        assert_eq!(empty.bounds(), (0.0, 1.0, 0.0, 1.0));
    }
}
