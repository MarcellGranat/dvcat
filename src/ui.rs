//! Drawing. Everything here takes `&App`: it can read state but never change it.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::App,
    project::{ParamRef, flatten_leaves, param_value},
};

const HELP: &str = " j/k move   q quit ";
/// Width of the label column in the detail pane ("depends", "metrics", ...).
const LABEL_WIDTH: usize = 9;
const WARNING: &str = "⚠";

pub fn view(app: &App, frame: &mut Frame) {
    // Split the screen: a big area on top, one line for help at the bottom.
    let [main, help] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    // Split the main area into a list (left) and a detail pane (right).
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main);

    render_stage_list(app, frame, left);
    render_detail(app, frame, right);
    frame.render_widget(Paragraph::new(HELP).dim(), help);
}

fn render_stage_list(app: &App, frame: &mut Frame, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = (0..app.stage_count())
        .filter_map(|position| app.stage_at(position))
        .map(|stage| match stage.unsupported {
            Some(_) => ListItem::new(format!("{} {WARNING}", stage.name)),
            None => ListItem::new(stage.name.as_str()),
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" Stages "))
        .highlight_style(Style::new().reversed())
        .highlight_symbol("▶ ");
    // ListState is a small scratch value rebuilt each frame from our own state.
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(app: &App, frame: &mut Frame, area: ratatui::layout::Rect) {
    let title = app
        .selected_stage()
        .map_or(" Detail ".to_owned(), |s| format!(" {} ", s.name));
    let detail = Paragraph::new(detail_lines(app))
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

/// The text of the detail pane. Lines own their text (`'static`), so they do not borrow from `app`.
fn detail_lines(app: &App) -> Vec<Line<'static>> {
    let Some(stage) = app.selected_stage() else {
        return vec![Line::from("(no stages in dvc.yaml)")];
    };

    let mut lines = Vec::new();
    lines.extend(section("cmd", vec![stage.cmd.clone()]));

    let upstream: Vec<String> = app
        .selected_upstream()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    lines.extend(section(
        "depends",
        if upstream.is_empty() {
            vec!["(none)".to_owned()]
        } else {
            vec![upstream.join(", ")]
        },
    ));

    lines.extend(section("deps", stage.deps.clone()));
    let params = stage
        .params
        .iter()
        .flat_map(|param| param_lines(app, param))
        .collect();
    lines.extend(section("params", params));
    lines.extend(section("outs", stage.outs.clone()));
    lines.extend(section("metrics", stage.metrics.clone()));

    if let Some(reason) = &stage.unsupported {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("{WARNING} {reason}"),
            Style::new().fg(Color::Yellow),
        ));
    }
    lines
}

/// One labelled block: the label sits on the first line, values are listed one per line below it.
/// An empty `values` produces no lines at all.
fn section(label: &str, values: Vec<String>) -> Vec<Line<'static>> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let label = if i == 0 { label } else { "" };
            Line::from(vec![
                Span::styled(
                    format!("{label:<LABEL_WIDTH$}"),
                    Style::new().add_modifier(Modifier::DIM | Modifier::BOLD),
                ),
                Span::raw(value),
            ])
        })
        .collect()
}

/// `params.yaml  train.lr = 0.01`, one line per tracked value.
/// A tracked section (`download`) is expanded into one line per leaf (`download.limit = null`).
fn param_lines(app: &App, param: &ParamRef) -> Vec<String> {
    if param.keys.is_empty() {
        return vec![format!("{}  (whole file)", param.file)];
    }
    let file = &param.file;
    param
        .keys
        .iter()
        .flat_map(|key| match app.project.params.get(file) {
            Some(Ok(tree)) => match param_value(tree, key) {
                Some(value) => flatten_leaves(key, value)
                    .into_iter()
                    .map(|(leaf_key, leaf)| format!("{file}  {leaf_key} = {leaf}"))
                    .collect(),
                None => vec![format!("{file}  {key} = (not found)")],
            },
            Some(Err(_)) => vec![format!("{file}  {key} = (file unreadable)")],
            None => vec![format!("{file}  {key} = (unknown file)")],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::test_support::{app_with_files, demo_app};

    /// Draw into an in-memory terminal and return its rows as plain text.
    fn render(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| view(app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
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

    fn screen(app: &App) -> String {
        render(app, 100, 20).join("\n")
    }

    #[test]
    fn list_shows_stages_in_dependency_order_with_selection_marker() {
        let yaml = "stages:\n  train:\n    cmd: t\n    deps: [f.csv]\n  features:\n    cmd: f\n    outs: [f.csv]\n";
        let (_dir, app) = app_with_files(&[("dvc.yaml", yaml)]);
        let rows = render(&app, 100, 20);
        // Row 0 is the border; rows 1.. are list items.
        assert!(rows[1].starts_with("│▶ features"), "{rows:#?}");
        assert!(rows[2].starts_with("│  train"), "{rows:#?}");
    }

    #[test]
    fn detail_shows_names_of_stages_it_depends_on() {
        let mut app = demo_app();
        assert!(screen(&app).contains("depends  (none)"), "{}", screen(&app));
        app.selected = 2; // train
        assert!(
            screen(&app).contains("depends  features"),
            "{}",
            screen(&app)
        );
    }

    #[test]
    fn detail_updates_when_selection_changes() {
        let mut app = demo_app();
        assert!(screen(&app).contains("python3 src/prepare.py"));
        app.update(crate::app::Event::Key(
            ratatui::crossterm::event::KeyCode::Char('j').into(),
        ));
        let text = screen(&app);
        assert!(text.contains("python3 src/features.py"), "{text}");
        assert!(!text.contains("python3 src/prepare.py"), "{text}");
    }

    #[test]
    fn detail_lists_cmd_deps_params_with_current_values_outs_and_metrics() {
        let mut app = demo_app();
        app.selected = 2; // train
        let text = screen(&app);
        for expected in [
            "cmd      python3 src/train.py",
            "deps     src/train.py",
            "         data/features.csv",
            "params   params.yaml  train.lr = 0.01",
            "         params.yaml  train.epochs = 5",
            "outs     model.txt",
            "metrics  metrics.json",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn params_report_missing_keys_files_and_whole_file_tracking() {
        let yaml = "stages:\n  a:\n    cmd: x\n    params:\n      - sec.present\n      - sec.absent\n      - gone.json:\n          - k\n      - params.yaml:\n";
        let (_dir, app) = app_with_files(&[
            ("dvc.yaml", yaml),
            ("params.yaml", "sec:\n  present: hello\n"),
        ]);
        let text = screen(&app);
        assert!(text.contains("sec.present = \"hello\""), "{text}");
        assert!(text.contains("sec.absent = (not found)"), "{text}");
        assert!(text.contains("gone.json  k = (file unreadable)"), "{text}");
        assert!(text.contains("params.yaml  (whole file)"), "{text}");
    }

    #[test]
    fn tracked_section_is_expanded_into_one_line_per_leaf() {
        let app = crate::test_support::app_at(&crate::test_support::fixture("articleiv"));
        let rows = render(&app, 100, 20);
        let text = rows.join("\n");
        assert!(
            text.contains("params.json  download.limit = null"),
            "{text}"
        );
        assert!(
            text.contains("params.json  download.page_size = 100"),
            "{text}"
        );
        assert!(
            text.contains("params.json  download.start_year = 2001"),
            "{text}"
        );
        assert!(!text.contains('{'), "no raw JSON blob expected:\n{text}");
    }

    #[test]
    fn unsupported_stage_is_flagged_in_list_and_detail() {
        let yaml = "stages:\n  each:\n    foreach: [a]\n    do:\n      cmd: echo\n";
        let (_dir, app) = app_with_files(&[("dvc.yaml", yaml)]);
        let text = screen(&app);
        assert!(text.contains("each ⚠"), "{text}");
        assert!(
            text.contains("foreach/matrix stages are not expanded"),
            "{text}"
        );
    }

    #[test]
    fn empty_project_shows_a_message_instead_of_crashing() {
        let (_dir, app) = app_with_files(&[("dvc.yaml", "stages: {}\n")]);
        assert!(screen(&app).contains("(no stages in dvc.yaml)"));
    }
}
