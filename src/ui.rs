//! Drawing. Everything here takes `&App`: it can read state but never change it.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, Run, RunPhase},
    project::{ParamRef, flatten_leaves, param_value},
    runner::RunOutcome,
    stale::Status,
};

const HELP: &str = " j/k move  r run  R run+upstream  x cancel  PgUp/PgDn log  q quit    ✓ ok  ● stale  ○ never run  ? unverified ";
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
    match &app.run {
        None => render_detail(app, frame, right),
        Some(run) => {
            let [top, bottom] =
                Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(right);
            render_detail(app, frame, top);
            render_log(run, frame, bottom);
        }
    }
    let footer = match &app.notice {
        Some(notice) => Paragraph::new(format!(" {notice} ")).style(Style::new().fg(Color::Yellow)),
        None => Paragraph::new(HELP).dim(),
    };
    frame.render_widget(footer, help);
}

fn render_log(run: &Run, frame: &mut Frame, area: ratatui::layout::Rect) {
    let (phase_text, color) = match &run.phase {
        RunPhase::Running => ("running…".to_owned(), Color::Cyan),
        RunPhase::Cancelling => ("cancelling…".to_owned(), Color::Yellow),
        RunPhase::Finished(RunOutcome::Succeeded) => ("✓ succeeded".to_owned(), Color::Green),
        RunPhase::Finished(RunOutcome::Failed { code: Some(code) }) => {
            (format!("✗ failed (exit {code})"), Color::Red)
        }
        RunPhase::Finished(RunOutcome::Failed { code: None }) => {
            ("✗ failed (ended by a signal)".to_owned(), Color::Red)
        }
        RunPhase::Finished(RunOutcome::Cancelled) => ("cancelled".to_owned(), Color::Yellow),
        RunPhase::Finished(RunOutcome::NotStarted(_)) => ("✗ not started".to_owned(), Color::Red),
    };
    let scrolled = if run.scroll > 0 {
        format!(" · {} lines back", run.scroll)
    } else {
        String::new()
    };
    let title = format!(" {} · {phase_text}{scrolled} ", run.command_line());

    // Show the window of lines that ends `scroll` lines above the bottom.
    let visible_rows = usize::from(area.height.saturating_sub(2));
    let all: Vec<&str> = run.log.lines().collect();
    let end = all.len().saturating_sub(run.scroll);
    let start = end.saturating_sub(visible_rows);
    let text = all[start..end].join("\n");

    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(color));
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_stage_list(app: &App, frame: &mut Frame, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = (0..app.stage_count())
        .filter_map(|position| {
            let stage = app.stage_at(position)?;
            let (glyph, _, color) = badge(app.status_at(position).map(|s| s.status()));
            let warning = if stage.unsupported.is_some() {
                format!(" {WARNING}")
            } else {
                String::new()
            };
            Some(ListItem::new(Line::from(vec![
                Span::styled(glyph, Style::new().fg(color)),
                Span::raw(format!(" {}{warning}", stage.name)),
            ])))
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
    let status = app.status_at(app.selected);
    let (glyph, label, color) = badge(status.map(|s| s.status()));
    lines.push(Line::from(vec![
        label_span("status"),
        Span::styled(format!("{glyph} {label}"), Style::new().fg(color)),
    ]));
    if let Some(status) = status {
        let why = status.reasons.iter().map(ToString::to_string).collect();
        lines.extend(section("why", why));
    }
    lines.push(Line::from(""));
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

fn label_span(label: &str) -> Span<'static> {
    Span::styled(
        format!("{label:<LABEL_WIDTH$}"),
        Style::new().add_modifier(Modifier::DIM | Modifier::BOLD),
    )
}

/// Glyph, text and colour for a status; `None` means the check has not finished yet.
fn badge(status: Option<Status>) -> (&'static str, &'static str, Color) {
    match status {
        None => ("…", "checking", Color::DarkGray),
        Some(Status::UpToDate) => ("✓", "up to date", Color::Green),
        Some(Status::Stale) => ("●", "stale", Color::Yellow),
        Some(Status::NeverRun) => ("○", "never run", Color::Gray),
        Some(Status::Unverified) => ("?", "unverified", Color::Magenta),
    }
}

/// One labelled block: the label sits on the first line, values are listed one per line below it.
/// An empty `values` produces no lines at all.
fn section(label: &str, values: Vec<String>) -> Vec<Line<'static>> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let label = if i == 0 { label } else { "" };
            Line::from(vec![label_span(label), Span::raw(value)])
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
    use crate::{
        app::{Run, RunPhase},
        runner::RunOutcome,
        test_support::{app_at, app_with_files, demo_app, fixture, with_statuses},
    };

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
        // No stale-check result has arrived yet, so each row shows the pending marker.
        assert!(rows[1].starts_with("│▶ … features"), "{rows:#?}");
        assert!(rows[2].starts_with("│  … train"), "{rows:#?}");
    }

    #[test]
    fn list_and_detail_show_pending_state_until_statuses_arrive() {
        let text = screen(&demo_app());
        assert!(text.contains("status   … checking"), "{text}");
    }

    #[test]
    fn up_to_date_stages_get_a_check_mark() {
        let mut app = with_statuses(demo_app());
        let rows = render(&app, 100, 20);
        assert!(rows[1].starts_with("│▶ ✓ prepare"), "{rows:#?}");
        assert!(rows[3].starts_with("│  ✓ train"), "{rows:#?}");
        app.selected = 2;
        assert!(screen(&app).contains("status   ✓ up to date"));
    }

    #[test]
    fn never_run_stage_gets_an_open_circle_and_reason() {
        let (_dir, app) = app_with_files(&[("dvc.yaml", "stages:\n  a:\n    cmd: x\n")]);
        let app = with_statuses(app);
        let text = screen(&app);
        assert!(text.contains("○ a"), "{text}");
        assert!(text.contains("status   ○ never run"), "{text}");
        assert!(text.contains("why      never run"), "{text}");
    }

    #[test]
    fn stale_stage_shows_the_param_change_on_screen() {
        let app = with_statuses(app_at(&fixture("articleiv")));
        let text = screen(&app);
        assert!(text.contains("● download"), "{text}");
        assert!(text.contains("status   ● stale"), "{text}");
        // (This fixture has no source files, so "missing:" reasons appear above it.)
        assert!(text.contains("download.limit: 3 → null"), "{text}");
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

    fn app_with_run(output: &str, phase: RunPhase) -> App {
        let mut app = demo_app();
        let mut run = Run::new("train".into(), false);
        run.log.push_str(output);
        run.phase = phase;
        app.run = Some(run);
        app
    }

    #[test]
    fn running_log_shows_the_command_and_only_the_latest_progress_state() {
        let app = app_with_run("start\nprogress 20%\rprogress 60%", RunPhase::Running);
        let text = screen(&app);
        assert!(
            text.contains("dvc repro --single-item train · running…"),
            "{text}"
        );
        assert!(text.contains("start"), "{text}");
        assert!(text.contains("progress 60%"), "{text}");
        assert!(!text.contains("progress 20%"), "{text}");
        // The stage detail is still visible above the log.
        assert!(text.contains("cmd      python3 src/prepare.py"), "{text}");
    }

    #[test]
    fn finished_runs_show_their_outcome_in_the_log_title() {
        let cases = [
            (RunOutcome::Succeeded, "✓ succeeded"),
            (RunOutcome::Failed { code: Some(2) }, "✗ failed (exit 2)"),
            (
                RunOutcome::Failed { code: None },
                "✗ failed (ended by a signal)",
            ),
            (RunOutcome::Cancelled, "cancelled"),
            (
                RunOutcome::NotStarted("dvc not found".into()),
                "✗ not started",
            ),
        ];
        for (outcome, expected) in cases {
            let app = app_with_run("x\n", RunPhase::Finished(outcome));
            let text = screen(&app);
            assert!(text.contains(expected), "{expected:?} missing in:\n{text}");
        }
        let cancelling = screen(&app_with_run("x\n", RunPhase::Cancelling));
        assert!(cancelling.contains("cancelling…"));
    }

    #[test]
    fn log_follows_the_tail_and_can_be_scrolled_back() {
        let numbered: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let mut app = app_with_run(&numbered, RunPhase::Finished(RunOutcome::Succeeded));
        let text = screen(&app);
        assert!(text.contains("line 100"), "{text}");
        assert!(
            !text.contains("line 1\n") && !text.contains("line 50"),
            "{text}"
        );

        app.run.as_mut().unwrap().scroll = 40;
        let text = screen(&app);
        assert!(text.contains("line 60"), "{text}");
        assert!(!text.contains("line 100"), "{text}");
        assert!(text.contains("40 lines back"), "{text}");
    }

    #[test]
    fn not_started_message_is_visible_in_the_log() {
        let mut app = demo_app();
        let mut run = Run::new("train".into(), false);
        run.log.push_str("dvc not found. Looked in: here; there");
        run.log.finish();
        run.phase = RunPhase::Finished(RunOutcome::NotStarted("x".into()));
        app.run = Some(run);
        assert!(screen(&app).contains("Looked in: here; there"));
    }

    #[test]
    fn footer_shows_a_notice_instead_of_the_key_help() {
        let mut app = demo_app();
        let rows = render(&app, 100, 20);
        assert!(rows[19].contains("r run"), "{rows:#?}");
        app.notice = Some("a run is already in progress".into());
        let rows = render(&app, 100, 20);
        assert!(
            rows[19].contains("a run is already in progress"),
            "{rows:#?}"
        );
        assert!(!rows[19].contains("r run"));
    }
}
