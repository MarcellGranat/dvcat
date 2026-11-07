//! The experiments table: one row per experiment, a column per param or metric.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::Span,
    widgets::{
        Axis, Cell, Chart, Dataset, GraphType, Paragraph, Row as TableRow, Table as TableWidget,
        TableState, Wrap,
    },
};

use crate::{
    app::{App, ExperimentsView},
    experiments::{
        Change, Column, Direction, Row, RowKind, Table, baseline_of, cell_text, metric_delta,
        param_differs,
    },
    plot, theme,
};

/// The plot needs this much height in the experiments area, else it is not drawn.
const PLOT_MIN_AREA_HEIGHT: u16 = 12;
const PLOT_MAX_HEIGHT: u16 = 14;
const MAX_COLUMN_WIDTH: usize = 28;
const COLUMN_SPACING: usize = 2;
const ELLIPSIS: char = '…';
const HINT_NO_EXPERIMENTS: &str =
    "no experiments yet — create one with `dvc exp run -S <param>=<value>`";

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let view = &app.experiments;
    let block = super::pane(title(app), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match &view.data {
        None if view.loading => {
            let text = "loading… (running `dvc exp show --json`)";
            frame.render_widget(Paragraph::new(text).style(theme::muted()), inner);
        }
        None => {
            let text = "Experiments are not loaded yet. Press F5 to load them.";
            frame.render_widget(Paragraph::new(text).style(theme::muted()), inner);
        }
        Some(Err(message)) => {
            let text = format!("{message}\n\nPress F5 to try again.");
            let error = Paragraph::new(text)
                .style(Style::new().fg(theme::RED))
                .wrap(Wrap { trim: false });
            frame.render_widget(error, inner);
        }
        Some(Ok(table)) if view.plot && inner.height >= PLOT_MIN_AREA_HEIGHT => {
            let chart_height = (inner.height / 2).min(PLOT_MAX_HEIGHT);
            let [table_area, chart_area] =
                Layout::vertical([Constraint::Min(4), Constraint::Length(chart_height)])
                    .areas(inner);
            render_table(view, table, frame, table_area);
            render_plot(view, table, frame, chart_area);
        }
        Some(Ok(table)) => render_table(view, table, frame, inner),
    }
}

fn title(app: &App) -> String {
    let mut title = String::from("Experiments");
    if app.experiments.loading {
        title.push_str(if app.experiments.data.is_some() {
            " · refreshing…"
        } else {
            " · loading…"
        });
    }
    if app.experiments.deltas {
        title.push_str(" · metrics as differences from the baseline");
    }
    if app.is_running() {
        title.push_str(" · stage run in progress (x cancels; refresh unavailable)");
    }
    title
}

fn render_table(view: &ExperimentsView, table: &Table, frame: &mut Frame, area: Rect) {
    let [table_area, info_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    let columns = view.columns(table);
    let rows = table.ordered(&view.sort);
    let widths = column_widths(view, &columns, &rows);
    let cursor_column = view.column.min(columns.len().saturating_sub(1));
    let shown = visible_columns(&widths, cursor_column, usize::from(table_area.width));

    let header = TableRow::new(shown.iter().map(|&i| {
        let mut text = columns[i].title().to_owned();
        if let Some(direction) = view.sort.direction_of(&columns[i]) {
            text = format!(
                "{} {}",
                fit(&text, widths[i].saturating_sub(2)),
                arrow(direction)
            );
        }
        let mut style = Style::new()
            .add_modifier(Modifier::BOLD)
            .fg(header_color(&columns[i]));
        if i == cursor_column {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        Cell::from(fit(&text, widths[i])).style(style)
    }));

    let body: Vec<TableRow> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let cells = shown.iter().map(|&i| {
                let (text, style) = body_cell(view, &rows, index, &columns[i]);
                Cell::from(fit(&text, widths[i])).style(style)
            });
            TableRow::new(cells).style(row_style(row))
        })
        .collect();

    let constraints = shown.iter().map(|&i| Constraint::Length(widths[i] as u16));
    let widget = TableWidget::new(body, constraints)
        .header(header)
        .column_spacing(COLUMN_SPACING as u16)
        .row_highlight_style(theme::selected());
    let mut state = TableState::default().with_selected(Some(view.row));
    frame.render_stateful_widget(widget, table_area, &mut state);

    let info = info_line(view, table, &columns[cursor_column], &rows);
    frame.render_widget(info, info_area);
}

/// The chart under the table: the chosen metric against the cursor param (or creation order).
fn render_plot(view: &ExperimentsView, table: &Table, frame: &mut Frame, area: Rect) {
    let metrics = plot::plottable_metrics(table);
    if metrics.is_empty() {
        let block = super::pane("Plot", false);
        let text = "Nothing to plot: a metric needs numeric values for at least two experiments.";
        frame.render_widget(
            Paragraph::new(text).style(theme::muted()).block(block),
            area,
        );
        return;
    }
    let metric = &metrics[view.plot_metric % metrics.len()];
    let columns = view.columns(table);
    let plot = plot::plot(table, &view.sort, columns.get(view.column), metric);

    let all: Vec<(f64, f64)> = plot.points.iter().map(|p| (p.x, p.y)).collect();
    let selected = plot.point_for_row(view.row);
    let chosen: Vec<(f64, f64)> = selected.map(|p| vec![(p.x, p.y)]).unwrap_or_default();

    let (x_min, x_max, y_min, y_max) = plot.bounds();
    let mut title = format!("{} vs {}", plot.y_title, plot.x_title);
    if let Some(point) = selected {
        title.push_str(&format!(" · {}", point.label));
    }
    if metrics.len() > 1 {
        title.push_str(&format!(
            " · metric {}/{} ([ ])",
            view.plot_metric % metrics.len() + 1,
            metrics.len()
        ));
    }
    let datasets = vec![
        Dataset::default()
            .marker(Marker::Dot)
            .graph_type(GraphType::Scatter)
            .style(Style::new().fg(theme::SAPPHIRE))
            .data(&all),
        Dataset::default()
            .marker(Marker::Block)
            .graph_type(GraphType::Scatter)
            .style(Style::new().fg(theme::MAUVE))
            .data(&chosen),
    ];
    let axis = |title: &str, low: f64, high: f64| {
        Axis::default()
            .title(Span::styled(title.to_owned(), theme::muted()))
            .style(Style::new().fg(theme::SURFACE2))
            .labels([short_number(low), short_number(high)])
            .bounds([low, high])
    };
    let chart = Chart::new(datasets)
        .block(super::pane(title, false))
        .x_axis(axis(&plot.x_title, x_min, x_max))
        .y_axis(axis(&plot.y_title, y_min, y_max));
    frame.render_widget(chart, area);
}

/// At most four decimals, without trailing zeros.
pub(super) fn short_number(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn info_line<'a>(
    view: &ExperimentsView,
    table: &Table,
    column: &Column,
    rows: &[&Row],
) -> Paragraph<'a> {
    let selected = rows.get(view.row);
    if let Some(error) = selected.and_then(|row| row.error.as_ref()) {
        return Paragraph::new(format!("⚠ {error}")).style(Style::new().fg(theme::RED));
    }
    if !table.has_experiments() {
        return Paragraph::new(HINT_NO_EXPERIMENTS).style(Style::new().fg(theme::YELLOW));
    }
    let mut parts = vec![format!("column: {}", column.title())];
    if let Some(row) = selected {
        parts.push(format!("row: {}", row.label));
        if !matches!(column, Column::Name)
            && let Some(baseline) = baseline_of(rows, view.row)
        {
            parts.push(format!("baseline: {}", cell_text(baseline, column)));
        }
        if let Some(timestamp) = &row.timestamp {
            parts.push(timestamp.replace('T', " "));
        }
    }
    let hidden = table
        .param_columns
        .iter()
        .filter(|label| view.hidden.contains(*label))
        .count();
    if hidden > 0 {
        parts.push(format!("{hidden} params hidden (v)"));
    }
    Paragraph::new(parts.join("  ·  ")).style(theme::muted())
}

fn name_cell(row: &Row) -> String {
    let warning = if row.error.is_some() { " ⚠" } else { "" };
    match row.kind {
        RowKind::Experiment => format!("└ {}{warning}", row.label),
        _ => format!("{}{warning}", row.label),
    }
}

fn row_style(row: &Row) -> Style {
    match row.kind {
        RowKind::Workspace => Style::new()
            .fg(theme::SAPPHIRE)
            .add_modifier(Modifier::BOLD),
        RowKind::Baseline => Style::new()
            .fg(theme::LAVENDER)
            .add_modifier(Modifier::BOLD),
        RowKind::Experiment => Style::new().fg(theme::TEXT),
    }
}

fn header_color(column: &Column) -> Color {
    match column {
        Column::Name => theme::TEXT,
        Column::Param(_) => theme::PEACH,
        Column::Metric(_) => theme::GREEN,
        Column::Duration => theme::SUBTEXT0,
    }
}

fn arrow(direction: Direction) -> char {
    match direction {
        Direction::Ascending => '▲',
        Direction::Descending => '▼',
    }
}

/// Shorten `text` to at most `width` characters, ending in `…` when something was cut.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}{ELLIPSIS}")
}

/// The text and style of one body cell. Experiments are compared with their baseline: a param that
/// differs is highlighted, and in delta mode a numeric metric shows the difference instead.
fn body_cell(
    view: &ExperimentsView,
    rows: &[&Row],
    index: usize,
    column: &Column,
) -> (String, Style) {
    let row = rows[index];
    if matches!(column, Column::Name) {
        return (name_cell(row), Style::new());
    }
    let text = cell_text(row, column);
    let baseline = baseline_of(rows, index);
    match column {
        Column::Param(_) if param_differs(row, baseline, column) => (
            text,
            Style::new().fg(theme::PEACH).add_modifier(Modifier::BOLD),
        ),
        Column::Metric(label) if view.deltas => {
            match baseline.and_then(|baseline| metric_delta(row, baseline, label)) {
                Some((change, delta)) => {
                    let (arrow, color) = match change {
                        Change::Up => ("▲ ", theme::SAPPHIRE),
                        Change::Down => ("▼ ", theme::MAUVE),
                        Change::Same => ("", theme::OVERLAY1),
                    };
                    (format!("{arrow}{delta}"), Style::new().fg(color))
                }
                None => (text, Style::new()),
            }
        }
        _ => (text, Style::new()),
    }
}

/// Each column is as wide as its widest cell or title (plus room for a sort arrow), up to a cap.
fn column_widths(view: &ExperimentsView, columns: &[Column], rows: &[&Row]) -> Vec<usize> {
    columns
        .iter()
        .map(|column| {
            let cells =
                (0..rows.len()).map(|index| body_cell(view, rows, index, column).0.chars().count());
            let title = column.title().chars().count() + 2;
            cells
                .chain([title])
                .max()
                .unwrap_or(0)
                .min(MAX_COLUMN_WIDTH)
        })
        .collect()
}

/// Which columns to draw in `available` characters: the name column always, then a window of the
/// others that includes the cursor column. Returns indices into `widths`.
pub fn visible_columns(widths: &[usize], cursor: usize, available: usize) -> Vec<usize> {
    if widths.is_empty() {
        return Vec::new();
    }
    let cost = |index: usize| COLUMN_SPACING + widths[index];
    let name_width = widths[0].min(available);
    let target = cursor.clamp(1, widths.len().saturating_sub(1).max(1));

    // Slide the window right until the cursor column fits.
    let mut start = 1;
    while start < target && name_width + (start..=target).map(cost).sum::<usize>() > available {
        start += 1;
    }

    let mut shown = vec![0];
    let mut used = name_width;
    for index in start..widths.len() {
        // Columns up to the cursor are always shown; later ones only while they fit.
        if index > target && used + cost(index) > available {
            break;
        }
        used += cost(index);
        shown.push(index);
    }
    shown
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::{app::View, experiments, test_support::demo_app};

    fn two() -> Table {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        experiments::parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn none() -> Table {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/none.json");
        experiments::parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn app_showing(data: Option<Result<Table, String>>) -> App {
        let mut app = demo_app();
        app.view = View::Experiments;
        app.experiments.data = data;
        app
    }

    fn draw(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::view(app, frame)).unwrap();
        terminal
    }

    fn rows_of(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let (width, height) = (buffer.area.width, buffer.area.height);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        rows_of(&draw(app, width, height))
    }

    fn line_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
        rows.iter()
            .find(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?} in:\n{}", rows.join("\n")))
    }

    // ---- the layout helper ----

    #[test]
    fn name_column_is_always_shown_and_everything_fits_when_there_is_room() {
        assert_eq!(visible_columns(&[10, 5, 5, 5], 0, 100), [0, 1, 2, 3]);
        assert_eq!(visible_columns(&[10], 0, 100), [0]);
        assert_eq!(visible_columns(&[], 0, 100), Vec::<usize>::new());
    }

    #[test]
    fn the_window_slides_so_the_cursor_column_stays_visible() {
        let widths = [10, 8, 8, 8, 8, 8];
        // 10 + 2 spacing-and-8 per column: room for the name and two more columns.
        assert_eq!(visible_columns(&widths, 1, 32), [0, 1, 2]);
        assert_eq!(visible_columns(&widths, 5, 32), [0, 4, 5]);
        let shown = visible_columns(&widths, 3, 32);
        assert!(shown.contains(&0) && shown.contains(&3), "{shown:?}");
    }

    #[test]
    fn a_column_wider_than_the_screen_is_still_shown() {
        assert_eq!(visible_columns(&[10, 50], 1, 20), [0, 1]);
    }

    #[test]
    fn fit_truncates_with_an_ellipsis() {
        assert_eq!(fit("short", 10), "short");
        assert_eq!(fit("params.yaml:train.learning_rate", 12), "params.yaml…");
        assert_eq!(fit("abc", 0), "…");
    }

    // ---- the screen ----

    #[test]
    fn table_lists_workspace_baseline_then_experiments_with_param_and_metric_columns() {
        let app = app_showing(Some(Ok(two())));
        let rows = screen(&app, 150, 14);
        let header = line_with(&rows, "params.yaml:train.lr");
        assert!(
            header.contains("name") && header.contains("metrics.json:auc"),
            "{header}"
        );

        let order: Vec<usize> = ["workspace", "master", "└ lr-010", "└ lr-005"]
            .iter()
            .map(|name| {
                rows.iter()
                    .position(|r| r.contains(name))
                    .unwrap_or_else(|| panic!("{name} missing"))
            })
            .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "unexpected order {order:?}\n{}",
            rows.join("\n")
        );

        let fast = line_with(&rows, "lr-010");
        assert!(fast.contains("0.1") && fast.contains("0.9356"), "{fast}");
        let base = line_with(&rows, "master");
        assert!(base.contains("0.6633"), "{base}");
    }

    #[test]
    fn sorting_reorders_experiments_and_marks_the_column() {
        let mut app = app_showing(Some(Ok(two())));
        let auc = Column::Metric("metrics.json:auc".into());
        app.experiments.sort.choose(auc.clone());
        let rows = screen(&app, 150, 14);
        assert!(line_with(&rows, "metrics.json:auc").contains('▲'));
        let (a, b) = (
            rows.iter().position(|r| r.contains("lr-005")).unwrap(),
            rows.iter().position(|r| r.contains("lr-010")).unwrap(),
        );
        assert!(a < b, "ascending auc: lr-005 (0.85) before lr-010 (0.9356)");

        app.experiments.sort.choose(auc);
        let rows = screen(&app, 150, 14);
        assert!(line_with(&rows, "metrics.json:auc").contains('▼'));
        let (a, b) = (
            rows.iter().position(|r| r.contains("lr-005")).unwrap(),
            rows.iter().position(|r| r.contains("lr-010")).unwrap(),
        );
        assert!(b < a, "descending auc: lr-010 first");
    }

    #[test]
    fn the_selected_row_is_highlighted() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.row = 2; // lr-010
        let terminal = draw(&app, 150, 14);
        let rows = rows_of(&terminal);
        let y = rows.iter().position(|r| r.contains("lr-010")).unwrap() as u16;
        let highlighted =
            |y: u16| terminal.backend().buffer()[(5, y)].style().bg == Some(theme::SURFACE0);
        assert!(highlighted(y));
        assert!(!highlighted(y + 1));
    }

    #[test]
    fn hidden_params_have_no_column_and_the_info_line_counts_them() {
        let mut app = app_showing(Some(Ok(two())));
        let before = screen(&app, 150, 14).join("\n");
        assert!(before.contains("params.yaml:train.lr"), "{before}");
        app.experiments.hidden.insert("params.yaml:train.lr".into());
        let rows = screen(&app, 150, 14);
        assert!(
            !rows.join("\n").contains("params.yaml:train.lr"),
            "{rows:?}"
        );
        assert!(line_with(&rows, "column:").contains("1 params hidden (v)"));
    }

    #[test]
    fn info_line_names_the_cursor_column_and_row_in_full() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.column = 3; // params.yaml:train.lr
        app.experiments.row = 3;
        let rows = screen(&app, 150, 14);
        let info = line_with(&rows, "column:");
        assert!(
            info.contains("column: params.yaml:train.lr") && info.contains("row: lr-005"),
            "{info}"
        );
    }

    #[test]
    fn without_experiments_the_rows_remain_and_a_hint_explains_how_to_create_one() {
        let app = app_showing(Some(Ok(none())));
        let rows = screen(&app, 150, 14);
        assert!(rows.iter().any(|r| r.contains("workspace")));
        assert!(rows.iter().any(|r| r.contains("master")));
        assert!(
            rows.iter().any(|r| r.contains("dvc exp run")),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn narrow_terminals_keep_the_name_column_and_the_cursor_column() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.column = 5; // metrics.json:loss, the last column
        let rows = screen(&app, 60, 14);
        let header = line_with(&rows, "name");
        assert!(header.contains("metrics.json:loss"), "{header}");
        assert!(!header.contains("params.yaml:features.scale"), "{header}");
        assert!(
            rows.iter().any(|r| r.contains("workspace")),
            "name column stays"
        );
    }

    #[test]
    fn loading_error_and_not_loaded_states_have_clear_messages() {
        let mut app = app_showing(None);
        assert!(
            screen(&app, 100, 10)
                .join("\n")
                .contains("Press F5 to load")
        );
        app.experiments.loading = true;
        let text = screen(&app, 100, 10).join("\n");
        assert!(
            text.contains("loading…") && text.contains("dvc exp show --json"),
            "{text}"
        );

        let app = app_showing(Some(Err(
            "dvc exp show failed (exit 255):\nnot a git repository".into(),
        )));
        let text = screen(&app, 100, 10).join("\n");
        assert!(
            text.contains("not a git repository") && text.contains("Press F5 to try again"),
            "{text}"
        );
    }

    #[test]
    fn a_refresh_keeps_showing_the_old_table() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.loading = true;
        let text = screen(&app, 150, 14).join("\n");
        assert!(
            text.contains("refreshing…") && text.contains("lr-010"),
            "{text}"
        );
    }

    #[test]
    fn a_run_in_progress_is_announced_in_the_title() {
        let mut app = app_showing(Some(Ok(two())));
        app.run = Some(crate::app::Run::new("prepare".into(), false));
        assert!(
            screen(&app, 150, 14)
                .join("\n")
                .contains("stage run in progress")
        );
    }

    #[test]
    fn a_row_with_an_error_shows_the_message_in_the_info_line() {
        let json = r#"[{"rev":"workspace","name":null,"error":null,"experiments":null,"data":{"timestamp":null,"params":{},"metrics":{}}},
                      {"rev":"bad0000000","name":"broken","data":null,"experiments":null,"error":{"msg":"bad params.yaml"}}]"#;
        let mut app = app_showing(Some(Ok(experiments::parse(json).unwrap())));
        app.experiments.row = 1;
        let rows = screen(&app, 100, 10);
        assert!(line_with(&rows, "broken").contains('⚠'));
        assert!(line_with(&rows, "bad params.yaml").contains("⚠ bad params.yaml"));
    }

    // ---- differences from the baseline ----

    fn cell_style(
        terminal: &Terminal<TestBackend>,
        rows: &[String],
        needle: &str,
        line: &str,
    ) -> Style {
        let y = rows.iter().position(|r| r.contains(line)).unwrap() as u16;
        let x = rows[y as usize].find(needle).unwrap();
        let x = rows[y as usize][..x].chars().count() as u16;
        terminal.backend().buffer()[(x, y)].style()
    }

    #[test]
    fn changed_params_are_highlighted_and_equal_ones_are_not() {
        let app = app_showing(Some(Ok(two())));
        let terminal = draw(&app, 150, 14);
        let rows = rows_of(&terminal);
        // lr-010 has lr 0.1 (baseline 0.01) and epochs 8 (baseline 5); scale is unchanged.
        let lr = cell_style(&terminal, &rows, "0.1 ", "lr-010");
        assert_eq!(lr.fg, Some(theme::PEACH), "{rows:#?}");
        let scale = cell_style(&terminal, &rows, "2.0", "lr-010");
        assert_ne!(scale.fg, Some(theme::PEACH));
        // The baseline and workspace rows never highlight anything.
        let master = cell_style(&terminal, &rows, "0.01", "master");
        assert_ne!(master.fg, Some(theme::PEACH));
    }

    #[test]
    fn delta_mode_shows_signed_differences_for_experiments_only() {
        let mut app = app_showing(Some(Ok(two())));
        let before = screen(&app, 150, 14).join("\n");
        assert!(!before.contains('▲'), "{before}");
        app.experiments.deltas = true;
        let rows = rows(&app);
        let text = rows.join("\n");
        let lr010 = rows.iter().find(|r| r.contains("lr-010")).unwrap();
        assert!(lr010.contains("▲ +0.2723"), "{lr010}"); // 0.9356 - 0.6633
        let master = rows.iter().find(|r| r.contains("master")).unwrap();
        assert!(!master.contains('▲') && !master.contains('▼'), "{master}");
        assert!(text.contains("metrics as differences"), "{text}");
    }

    fn rows(app: &App) -> Vec<String> {
        let terminal = draw(app, 150, 14);
        rows_of(&terminal)
    }

    #[test]
    fn sorting_in_delta_mode_still_uses_the_real_values() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.deltas = true;
        app.experiments.column = app
            .experiments
            .table()
            .unwrap()
            .columns()
            .iter()
            .position(|c| matches!(c, Column::Metric(m) if m.ends_with("auc")))
            .unwrap();
        app.update(crate::app::Event::Key(
            ratatui::crossterm::event::KeyCode::Char('s').into(),
        ));
        let rows = rows(&app);
        let (a, b) = (
            rows.iter().position(|r| r.contains("lr-005")).unwrap(),
            rows.iter().position(|r| r.contains("lr-010")).unwrap(),
        );
        assert!(a < b, "ascending auc: lr-005 (0.85) before lr-010 (0.9356)");
    }

    #[test]
    fn info_line_names_the_baseline_value_for_an_experiment() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.row = 2;
        app.experiments.column = 3; // a param column
        let text = screen(&app, 150, 14).join("\n");
        assert!(text.contains("baseline: "), "{text}");
    }

    #[test]
    fn m_toggles_the_delta_mode() {
        let mut app = app_showing(Some(Ok(two())));
        let press = |app: &mut App| {
            app.update(crate::app::Event::Key(
                ratatui::crossterm::event::KeyCode::Char('m').into(),
            ));
        };
        press(&mut app);
        assert!(app.experiments.deltas);
        press(&mut app);
        assert!(!app.experiments.deltas);
    }

    // ---- plot panel ----

    fn press(app: &mut App, c: char) {
        app.update(crate::app::Event::Key(
            ratatui::crossterm::event::KeyCode::Char(c).into(),
        ));
    }

    fn column_of(app: &App, suffix: &str) -> usize {
        app.experiments
            .table()
            .unwrap()
            .columns()
            .iter()
            .position(|c| c.title().ends_with(suffix))
            .unwrap()
    }

    #[test]
    fn p_toggles_the_chart_without_touching_cursor_or_sort() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.row = 2;
        app.experiments.column = column_of(&app, "train.lr");
        let (row, column) = (app.experiments.row, app.experiments.column);
        assert!(!screen(&app, 140, 34).join("\n").contains(" vs "));
        press(&mut app, 'p');
        assert!(app.experiments.plot);
        assert!(screen(&app, 140, 34).join("\n").contains(" vs "));
        press(&mut app, 'p');
        assert!(!app.experiments.plot);
        assert_eq!((app.experiments.row, app.experiments.column), (row, column));
    }

    #[test]
    fn chart_title_names_both_axes_and_the_experiment_under_the_cursor() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.plot = true;
        app.experiments.column = column_of(&app, "train.lr");
        app.experiments.row = 3; // lr-010
        let text = screen(&app, 140, 34).join("\n");
        assert!(
            text.contains("metrics.json:auc vs params.yaml:train.lr"),
            "{text}"
        );
        assert!(text.contains("lr-010"), "{text}");
        assert!(
            text.contains("█"),
            "the selected point is drawn as a block:\n{text}"
        );
    }

    #[test]
    fn the_marked_point_follows_the_table_cursor() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.plot = true;
        app.experiments.column = column_of(&app, "train.lr");
        let block_x = |app: &App| {
            let rows = screen(app, 140, 34);
            rows.iter().find_map(|r| r.chars().position(|c| c == '█'))
        };
        app.experiments.row = 2;
        let first = block_x(&app);
        app.experiments.row = 3;
        let second = block_x(&app);
        assert!(
            first.is_some() && second.is_some() && first != second,
            "{first:?} {second:?}"
        );
    }

    #[test]
    fn the_name_column_plots_creation_order() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.plot = true;
        app.experiments.column = 0;
        let text = screen(&app, 140, 34).join("\n");
        assert!(text.contains("vs creation order"), "{text}");
    }

    #[test]
    fn with_too_little_data_the_panel_says_so() {
        // One experiment only: no metric has two numeric points.
        let mut table = two();
        let mut seen = 0;
        table.rows.retain(|r| {
            if r.kind != crate::experiments::RowKind::Experiment {
                return true;
            }
            seen += 1;
            seen == 1
        });
        let mut app = app_showing(Some(Ok(table)));
        app.experiments.plot = true;
        let text = screen(&app, 140, 34).join("\n");
        assert!(text.contains("Nothing to plot"), "{text}");
        assert!(!text.contains(" vs "), "{text}");
    }

    #[test]
    fn bracket_keys_cycle_through_plottable_metrics() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.plot = true;
        let count = plot::plottable_metrics(app.experiments.table().unwrap()).len();
        assert!(count >= 2, "the sample has auc and loss");
        press(&mut app, ']');
        assert_eq!(app.experiments.plot_metric, 1);
        press(&mut app, '[');
        press(&mut app, '[');
        assert_eq!(app.experiments.plot_metric, count - 1, "wraps backwards");
        let text = screen(&app, 140, 34).join("\n");
        assert!(text.contains(&format!("metric {count}/{count}")), "{text}");
    }

    #[test]
    fn chart_is_skipped_on_short_terminals_and_never_panics() {
        let mut app = app_showing(Some(Ok(two())));
        app.experiments.plot = true;
        for (w, h) in [(140, 10), (30, 14), (10, 5), (1, 1), (140, 40)] {
            let _ = screen(&app, w, h);
        }
        let short = screen(&app, 140, 12).join("\n");
        assert!(
            !short.contains(" vs "),
            "table only when there is no room:\n{short}"
        );
    }
}
