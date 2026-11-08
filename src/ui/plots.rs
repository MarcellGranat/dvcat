//! The plots view: the plots from `dvc.yaml`, listed on the left and drawn on the right.

use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Chart, Clear, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::App,
    plot_form::{FIELDS, PlotForm},
    plots::{self, Content, Plot, Series, Template},
    theme,
};

const LIST_PERCENT: u16 = 28;
/// One colour per series, repeated when there are more series than colours.
const SERIES_COLORS: [Color; 6] = [
    theme::SAPPHIRE,
    theme::MAUVE,
    theme::GREEN,
    theme::PEACH,
    theme::RED,
    theme::TEAL,
];
const BAR_CHAR: char = '█';
const CELL_WIDTH: usize = 7;
const HINT_NO_PLOTS: &str = "No plots yet. Add a `plots:` section to dvc.yaml, for example:\n\n  plots:\n    - metrics/acc.csv\n    - loss:\n        x: epoch\n        y:\n          train.csv: loss\n          test.csv: loss\n\nCSV, TSV, JSON and YAML files are read from the workspace.";

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let plots = match &app.plots.data {
        Some(Ok(plots)) if !plots.is_empty() => plots,
        Some(Err(error)) => {
            let block = super::pane("Plots", true);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(format!("⚠ {error}"))
                    .style(Style::new().fg(theme::RED))
                    .wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        _ => {
            let block = super::pane("Plots", true);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(Paragraph::new(HINT_NO_PLOTS).style(theme::muted()), inner);
            return;
        }
    };

    let [list_area, plot_area] =
        Layout::horizontal([Constraint::Percentage(LIST_PERCENT), Constraint::Min(0)]).areas(area);
    let items: Vec<ListItem> = plots
        .iter()
        .map(|plot| {
            let mark = if matches!(plot.content, Content::Unavailable(_)) {
                "⚠ "
            } else {
                ""
            };
            ListItem::new(format!("{mark}{}", plot.id))
        })
        .collect();
    let list = List::new(items)
        .block(super::pane(format!("Plots ({})", plots.len()), true))
        .highlight_style(theme::selected());
    let mut state = ListState::default().with_selected(Some(app.plots.selected));
    frame.render_stateful_widget(list, list_area, &mut state);

    if let Some(plot) = plots.get(app.plots.selected) {
        if app.plots.definition {
            // The definition needs its lines plus the border; the chart keeps the rest.
            let wanted = plot.definition.lines().count() as u16 + 2;
            let height = wanted.min(plot_area.height / 2).max(3);
            let [chart_area, definition_area] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(height)]).areas(plot_area);
            render_plot(plot, frame, chart_area);
            render_definition(plot, frame, definition_area);
        } else {
            render_plot(plot, frame, plot_area);
        }
    }
}

fn render_plot(plot: &Plot, frame: &mut Frame, area: Rect) {
    let block = super::pane(plot.title.clone(), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match &plot.content {
        Content::Unavailable(reason) => frame.render_widget(
            Paragraph::new(format!("⚠ {reason}"))
                .style(Style::new().fg(theme::YELLOW))
                .wrap(Wrap { trim: false }),
            inner,
        ),
        Content::Lines(series) => render_lines(plot, series, frame, inner),
        Content::Bars(bars) => render_bars(plot, bars, frame, inner),
        Content::Matrix { labels, counts } => render_matrix(plot, labels, counts, frame, inner),
    }
}

fn color(index: usize) -> Color {
    SERIES_COLORS[index % SERIES_COLORS.len()]
}

fn render_lines(plot: &Plot, series: &[Series], frame: &mut Frame, area: Rect) {
    let [chart_area, legend_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let (x_min, x_max, y_min, y_max) = Plot::bounds(series);
    let (graph_type, marker) = match plot.template {
        Template::Scatter => (GraphType::Scatter, Marker::Dot),
        _ => (GraphType::Line, Marker::Braille),
    };
    let datasets: Vec<Dataset> = series
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Dataset::default()
                .marker(marker)
                .graph_type(graph_type)
                .style(Style::new().fg(color(i)))
                .data(&s.points)
        })
        .collect();
    let axis = |title: &str, low: f64, high: f64| {
        Axis::default()
            .title(Span::styled(title.to_owned(), theme::muted()))
            .style(Style::new().fg(theme::SURFACE2))
            .labels([
                super::experiments::short_number(low),
                super::experiments::short_number(high),
            ])
            .bounds([low, high])
    };
    let chart = Chart::new(datasets)
        .x_axis(axis(&plot.x_title, x_min, x_max))
        .y_axis(axis(&plot.y_title, y_min, y_max));
    frame.render_widget(chart, chart_area);

    let mut legend: Vec<Span> = Vec::new();
    for (i, s) in series.iter().enumerate() {
        legend.push(Span::styled("━ ", Style::new().fg(color(i))));
        legend.push(Span::styled(format!("{}  ", s.name), theme::muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(legend)), legend_area);
}

fn render_bars(plot: &Plot, bars: &[(String, f64)], frame: &mut Frame, area: Rect) {
    let label_width = bars
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    let top = bars.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    let value_width = 10;
    let room = (area.width as usize)
        .saturating_sub(label_width + value_width + 2)
        .max(1);
    let mut lines = vec![Line::from(Span::styled(
        format!("{} by {}", plot.x_title, plot.y_title),
        theme::muted(),
    ))];
    for (label, value) in bars {
        let length = if top > 0.0 {
            ((value / top).max(0.0) * room as f64).round() as usize
        } else {
            0
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{label:>label_width$} ")),
            Span::styled(
                BAR_CHAR.to_string().repeat(length),
                Style::new().fg(theme::SAPPHIRE),
            ),
            Span::styled(
                format!(" {}", super::experiments::short_number(*value)),
                theme::muted(),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_matrix(
    plot: &Plot,
    labels: &[String],
    counts: &[Vec<f64>],
    frame: &mut Frame,
    area: Rect,
) {
    let top = counts.iter().flatten().copied().fold(0.0_f64, f64::max);
    let cell = |text: String| format!("{text:>CELL_WIDTH$}");
    let mut lines = vec![
        Line::from(Span::styled(
            format!("rows: {} · columns: {}", plot.x_title, plot.y_title),
            theme::muted(),
        )),
        Line::from(
            std::iter::once(Span::raw(cell(String::new())))
                .chain(labels.iter().map(|l| {
                    Span::styled(
                        cell(l.chars().take(CELL_WIDTH - 1).collect()),
                        theme::muted(),
                    )
                }))
                .collect::<Vec<_>>(),
        ),
    ];
    for (i, row) in counts.iter().enumerate() {
        let mut spans = vec![Span::styled(
            cell(labels[i].chars().take(CELL_WIDTH - 1).collect()),
            theme::muted(),
        )];
        for (j, &count) in row.iter().enumerate() {
            let style = if count == 0.0 {
                Style::new().fg(theme::SURFACE2)
            } else if i == j {
                Style::new().fg(theme::GREEN)
            } else if top > 0.0 && count / top > 0.5 {
                Style::new().fg(theme::RED)
            } else {
                Style::new().fg(theme::PEACH)
            };
            spans.push(Span::styled(
                cell(super::experiments::short_number(count)),
                style,
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_definition(plot: &Plot, frame: &mut Frame, area: Rect) {
    let block = super::pane("Definition in dvc.yaml", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = plot
        .definition
        .lines()
        .map(|line| match line.split_once(':') {
            Some((key, value)) if !key.trim_start_matches([' ', '-']).is_empty() => {
                Line::from(vec![
                    Span::styled(format!("{key}:"), Style::new().fg(theme::PEACH)),
                    Span::styled(value.to_owned(), Style::new().fg(theme::TEXT)),
                ])
            }
            _ => Line::styled(line.to_owned(), Style::new().fg(theme::TEXT)),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The "new plot" form in the middle of the screen: a line per field, then a hint or the error.
pub fn render_form(form: &PlotForm, root: &Path, frame: &mut Frame, area: Rect) {
    let width = (area.width * 80 / 100).max(40).min(area.width);
    let height = (FIELDS.len() as u16 + 4).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = super::pane("New plot · added to dvc.yaml", true)
        .border_style(Style::new().fg(theme::MAUVE));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [rows_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);

    let label_width = FIELDS
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = FIELDS
        .iter()
        .enumerate()
        .map(|(index, (label, _))| {
            let active = index == form.active;
            let mut spans = vec![
                Span::styled(
                    if active { "▌ " } else { "  " },
                    Style::new().fg(theme::MAUVE),
                ),
                Span::styled(format!("{label:<label_width$}  "), theme::label()),
                Span::styled(form.values[index].clone(), Style::new().fg(theme::TEXT)),
            ];
            if active {
                spans.push(Span::styled("▏", Style::new().fg(theme::MAUVE)));
            }
            let line = Line::from(spans);
            if active {
                line.style(theme::selected())
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_area);

    let hint = FIELDS[form.active].1;
    let status = if let Some(error) = &form.error {
        Line::styled(format!("⚠ {error}"), Style::new().fg(theme::RED))
    } else if form.file().is_empty() {
        Line::styled(hint, theme::muted())
    } else {
        match plots::columns(root, form.file()) {
            Ok(columns) => Line::styled(
                format!("columns: {}", columns.join(", ")),
                Style::new().fg(theme::TEAL),
            ),
            Err(reason) => Line::styled(
                format!("{reason} (the plot is added anyway)"),
                Style::new().fg(theme::YELLOW),
            ),
        }
    };
    let second = Line::styled(hint, theme::muted().add_modifier(Modifier::ITALIC));
    frame.render_widget(Paragraph::new(vec![status, second]), status_area);
}
