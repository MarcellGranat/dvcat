//! Drawing. Everything here takes `&App`: it can read state but never change it.

use std::collections::BTreeSet;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

mod dag;
mod experiments;
mod plots;
mod stage_form;

use crate::{
    app::{App, Confirm, Focus, Run, RunPhase, View},
    files::{FileEntry, FileKind, Preview, PreviewBody},
    form::ExperimentForm,
    history::format_duration,
    params_filter::{ParamPicker, Shown},
    project::{ParamRef, flatten_leaves, param_value},
    prompt::Prompt,
    runner::RunOutcome,
    stale::Status,
    theme,
};

/// Width of the label column in the detail pane ("depends", "metrics", ...).
const LABEL_WIDTH: usize = 9;
const WARNING: &str = "⚠";
/// Key hints for the footer: `(key, what it does)`.
const PIPELINE_KEYS: &[(&str, &str)] = &[
    ("j/k", "move"),
    ("Tab", "files"),
    ("r", "run"),
    ("R", "run+upstream"),
    ("n", "new stage"),
    ("x", "cancel"),
    ("P/U", "pull/push"),
    ("PgUp/PgDn", "log"),
    ("g", "graph"),
    ("e", "experiments"),
    ("q", "quit"),
];
const FILES_KEYS: &[(&str, &str)] = &[
    ("j/k", "move"),
    ("Enter", "preview"),
    ("o", "editor"),
    ("Tab", "stages"),
    ("r", "run"),
    ("g", "graph"),
    ("e", "experiments"),
    ("q", "quit"),
];
const PREVIEW_KEYS: &[(&str, &str)] = &[
    ("j/k", "scroll"),
    ("PgUp/PgDn", "page"),
    ("g/G", "top/bottom"),
    ("o", "editor"),
    ("Esc", "close"),
];
const FORM_KEYS: &[(&str, &str)] = &[
    ("Tab/Shift+Tab", "next/previous field"),
    ("type", "edit"),
    ("Ctrl+U", "clear field"),
    ("Enter", "run"),
    ("Esc", "cancel"),
];
const PLOT_FORM_KEYS: &[(&str, &str)] = &[
    ("Tab", "next field"),
    ("Shift+Tab", "previous"),
    ("Ctrl+F", "pick file (fzf)"),
    ("Enter", "add to dvc.yaml"),
    ("Esc", "cancel"),
];
const STAGE_FORM_KEYS: &[(&str, &str)] = &[
    ("Tab", "next field"),
    ("Space", "toggle"),
    ("Ctrl+F", "pick (fzf)"),
    ("Enter", "add to dvc.yaml"),
    ("Esc", "cancel"),
];
const PROMPT_KEYS: &[(&str, &str)] = &[
    ("type", "edit"),
    ("Ctrl+U", "clear"),
    ("Enter", "ok"),
    ("Esc", "cancel"),
];
const PICKER_KEYS: &[(&str, &str)] = &[
    ("j/k", "move"),
    ("Space", "hide/show"),
    ("a", "show all"),
    ("Esc", "close"),
];
const CONFIRM_KEYS: &[(&str, &str)] = &[("y/Enter", "yes"), ("n/Esc", "no")];
const DAG_KEYS: &[(&str, &str)] = &[
    ("h/l", "upstream/downstream"),
    ("j/k", "same layer"),
    ("Enter", "open stage"),
    ("n", "new stage"),
    ("r", "run"),
    ("R", "run+upstream"),
    ("x", "cancel"),
    ("g/Esc", "back"),
    ("e", "experiments"),
    ("q", "quit"),
];
const PLOTS_KEYS: &[(&str, &str)] = &[
    ("j/k", "plot"),
    ("d", "definition"),
    ("n", "new plot"),
    ("F5", "reload"),
    ("1", "pipeline"),
    ("e", "experiments"),
    ("q", "quit"),
];
const EXPERIMENTS_KEYS: &[(&str, &str)] = &[
    ("j/k", "row"),
    ("h/l", "column"),
    ("s", "sort"),
    ("m", "deltas"),
    ("t", "transpose"),
    ("c", "comment"),
    ("p", "plot"),
    ("[ ]", "metric"),
    ("v", "params"),
    ("H", "hide col"),
    ("n", "new"),
    ("a", "apply"),
    ("b", "branch"),
    ("d", "remove"),
    ("P/U", "pull/push"),
    ("F5", "refresh"),
    ("x", "cancel run"),
    ("e/Esc", "pipeline"),
    ("q", "quit"),
];

/// A rounded pane. `focused` lights up its border and title.
fn pane<'a>(title: impl Into<String>, focused: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(Span::styled(
            format!(" {} ", title.into()),
            theme::title(focused),
        ))
}

pub fn view(app: &App, frame: &mut Frame) {
    // Paint the background first; everything else is drawn on top of it.
    frame.render_widget(Block::new().style(theme::base()), frame.area());

    // Top bar, a big area, one line of key hints.
    let [header, main, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_header(app, frame, header);

    let keys = match app.view {
        View::Pipeline => {
            render_pipeline(app, frame, main);
            match app.focus {
                Focus::Stages => PIPELINE_KEYS,
                Focus::Files => FILES_KEYS,
            }
        }
        View::Experiments => {
            experiments::render(app, frame, main);
            EXPERIMENTS_KEYS
        }
        View::Dag => {
            dag::render(app, frame, main);
            DAG_KEYS
        }
        View::Plots => {
            plots::render(app, frame, main);
            PLOTS_KEYS
        }
    };
    // The popup floats over everything else.
    let keys = match &app.preview {
        Some(preview) => {
            render_preview(preview, frame, main);
            PREVIEW_KEYS
        }
        None => keys,
    };

    let keys = match &app.form {
        Some(form) => {
            render_form(form, frame, main);
            FORM_KEYS
        }
        None => keys,
    };

    let keys = match &app.stage_form {
        Some(form) => {
            stage_form::render(form, frame, main);
            STAGE_FORM_KEYS
        }
        None => keys,
    };

    let keys = match &app.plot_form {
        Some(form) => {
            plots::render_form(form, &app.project.root, frame, main);
            PLOT_FORM_KEYS
        }
        None => keys,
    };

    let keys = match &app.param_picker {
        Some(picker) => {
            render_param_picker(picker, &app.experiments.hidden, frame, main);
            PICKER_KEYS
        }
        None => keys,
    };

    let keys = match &app.confirm {
        Some(confirm) => {
            render_confirm(confirm, frame, main);
            CONFIRM_KEYS
        }
        None => keys,
    };

    let keys = match &app.prompt {
        Some(prompt) => {
            render_prompt(prompt, frame, main);
            PROMPT_KEYS
        }
        None => keys,
    };

    let footer_line = match &app.notice {
        Some(notice) => Line::from(Span::styled(
            format!(" {notice} "),
            Style::new().fg(theme::YELLOW),
        )),
        None => key_hints(keys),
    };
    frame.render_widget(Paragraph::new(footer_line), footer);
}

/// `key description  key description ...`, keys bright and descriptions dim.
fn key_hints(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in keys {
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::new()
                .fg(theme::LAVENDER)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {what}  "), theme::muted()));
    }
    Line::from(spans)
}

/// App name and project on the left, the two views as tabs on the right.
fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let project = app.project.root.file_name().map_or_else(
        || app.project.root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut left = vec![
        Span::styled(
            " ◆ dvcat ",
            Style::new()
                .fg(theme::CRUST)
                .bg(theme::MAUVE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {project}"), Style::new().fg(theme::SUBTEXT0)),
    ];
    if let Some(run) = app.run.as_ref().filter(|run| run.is_active()) {
        left.push(Span::styled(
            format!("  ● running {}", run.stage),
            Style::new().fg(theme::PEACH),
        ));
    }
    if let Some(run) = app.external.first() {
        let age = run
            .age()
            .map(|d| format!(" · {}", format_duration(d)))
            .unwrap_or_default();
        let more = match app.external.len() {
            1 => String::new(),
            n => format!(" (+{})", n - 1),
        };
        left.push(Span::styled(
            format!(
                "  ⚠ outside dvcat: {}{age}{more} · X stops it",
                run.short_command()
            ),
            Style::new().fg(theme::YELLOW),
        ));
    }

    let tab = |label: &'static str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(theme::CRUST)
                    .bg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), theme::muted())
        }
    };
    let tabs = Line::from(vec![
        tab("1 Pipeline", app.view == View::Pipeline),
        tab("2 Graph (g)", app.view == View::Dag),
        tab("3 Experiments (e)", app.view == View::Experiments),
        tab("4 Plots (p)", app.view == View::Plots),
    ]);
    // `Line::width` counts the displayed width, so the tabs hug the right edge.
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(tabs.width() as u16 + 1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(Line::from(left)), left_area);
    frame.render_widget(Paragraph::new(tabs).alignment(Alignment::Right), right_area);
}

fn render_pipeline(app: &App, frame: &mut Frame, area: Rect) {
    // Split the area into a column of lists (left) and a detail pane (right).
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);

    let files = app.selected_files();
    // The files pane is as tall as its content, but never more than half the column.
    let files_height = (files.len() as u16 + 2).clamp(4, (left.height / 2).max(4));
    let [stages_area, files_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(files_height)]).areas(left);
    render_stage_list(app, frame, stages_area);
    render_files(app, &files, frame, files_area);

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
}

/// How a finished run is described, and in which colour.
fn outcome_label(outcome: &RunOutcome) -> (String, Color) {
    match outcome {
        RunOutcome::Succeeded => ("✓ succeeded".to_owned(), theme::GREEN),
        RunOutcome::Failed { code: Some(code) } => (format!("✗ failed (exit {code})"), theme::RED),
        RunOutcome::Failed { code: None } => {
            ("✗ failed (ended by a signal)".to_owned(), theme::RED)
        }
        RunOutcome::Cancelled => ("cancelled".to_owned(), theme::YELLOW),
        RunOutcome::NotStarted(_) => ("✗ not started".to_owned(), theme::RED),
    }
}

fn render_log(run: &Run, frame: &mut Frame, area: Rect) {
    let (phase_text, color) = match &run.phase {
        RunPhase::Running => ("running…".to_owned(), theme::SAPPHIRE),
        RunPhase::Cancelling => ("cancelling…".to_owned(), theme::YELLOW),
        RunPhase::Finished(outcome) => outcome_label(outcome),
    };
    let scrolled = if run.scroll > 0 {
        format!(" · {} lines back", run.scroll)
    } else {
        String::new()
    };
    // The scroll position matters more than the time, so the time goes last (a narrow pane cuts the end).
    let title = format!(
        "{} · {phase_text}{scrolled} · {}",
        run.command_line(),
        format_duration(run.duration())
    );

    // Show the window of lines that ends `scroll` lines above the bottom.
    let visible_rows = usize::from(area.height.saturating_sub(2));
    let all: Vec<&str> = run.log.lines().collect();
    let end = all.len().saturating_sub(run.scroll);
    let start = end.saturating_sub(visible_rows);
    let text = all[start..end].join("\n");

    // The border colour tells the outcome at a glance.
    let block = pane(title, false)
        .border_style(Style::new().fg(color))
        .title_style(Style::new().fg(color).add_modifier(Modifier::BOLD));
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::new().fg(theme::SUBTEXT0))
            .block(block),
        area,
    );
}

fn render_stage_list(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Stages;
    // Inside the border and the two-cell selection marker.
    let list_width = usize::from(area.width.saturating_sub(4));
    let items: Vec<ListItem> = (0..app.stage_count())
        .filter_map(|position| {
            let stage = app.stage_at(position)?;
            let (glyph, _, color) = badge(app.status_at(position).map(|s| s.status()));
            let warning = if stage.unsupported.is_some() {
                format!(" {WARNING}")
            } else {
                String::new()
            };
            let outside = if app.external_on(position).is_some() {
                " ⚠ running outside dvcat"
            } else {
                ""
            };
            let left = format!(" {}{warning}{outside}", stage.name);
            let mut spans = vec![
                Span::styled(glyph, Style::new().fg(color)),
                Span::raw(left.clone()),
            ];
            // How long its last run took, right-aligned (when the pane is wide enough).
            if let Some(last) = app.last_runs.get(&stage.name) {
                let time = format_duration(last.duration);
                let used = glyph.chars().count() + left.chars().count();
                if let Some(gap) = list_width.checked_sub(used + time.chars().count() + 1) {
                    spans.push(Span::raw(" ".repeat(gap + 1)));
                    spans.push(Span::styled(time, theme::muted()));
                }
            }
            Some(ListItem::new(Line::from(spans)))
        })
        .collect();
    let list = List::new(items)
        .block(pane("Stages", focused))
        .highlight_style(if focused {
            theme::selected()
        } else {
            Style::new().fg(theme::LAVENDER)
        })
        .highlight_symbol("▌ ");
    // ListState is a small scratch value rebuilt each frame from our own state.
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_files(app: &App, files: &[FileEntry], frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Files;
    let name = app.selected_stage().map_or("", |stage| stage.name.as_str());
    let block = pane(format!("Files · {name}"), focused);
    if files.is_empty() {
        frame.render_widget(
            Paragraph::new("(no files)")
                .style(theme::muted())
                .block(block),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = files
        .iter()
        .map(|file| {
            Line::from(vec![
                Span::styled(
                    format!("{:<7}", file.kind.label()),
                    Style::new().fg(kind_color(file.kind)),
                ),
                Span::raw(file.path.clone()),
            ])
            .into()
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme::selected())
        .highlight_symbol("▌ ");
    // Only the focused pane shows a cursor; otherwise nothing looks selectable.
    let mut state = ListState::default().with_selected(focused.then_some(app.file_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

pub(super) fn kind_color(kind: FileKind) -> Color {
    match kind {
        FileKind::Dep => theme::BLUE,
        FileKind::Param => theme::PEACH,
        FileKind::Out => theme::GREEN,
        FileKind::Metric => theme::TEAL,
    }
}

/// A centred popup showing one file. `Clear` wipes what is underneath so nothing bleeds through.
fn render_preview(preview: &Preview, frame: &mut Frame, area: Rect) {
    let width = (area.width * 85 / 100).max(20).min(area.width);
    let height = (area.height * 85 / 100).max(5).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);

    let (subtitle, lines): (String, Vec<String>) = match &preview.body {
        PreviewBody::Text { text, truncated } => (
            if *truncated {
                "first 64 KiB".into()
            } else {
                String::new()
            },
            text.lines().map(str::to_owned).collect(),
        ),
        PreviewBody::Directory { names, truncated } => (
            format!(
                "directory · {} entries{}",
                names.len(),
                if *truncated { "+" } else { "" }
            ),
            names.clone(),
        ),
        PreviewBody::Binary { bytes } => (
            "binary".into(),
            vec![format!(
                "binary file, {bytes} bytes — press o to open it in your editor"
            )],
        ),
        PreviewBody::Missing => (
            "missing".into(),
            vec!["This file does not exist (yet). Outputs appear after the stage has run.".into()],
        ),
        PreviewBody::Unreadable(error) => {
            ("unreadable".into(), vec![format!("cannot read: {error}")])
        }
    };
    let title = if subtitle.is_empty() {
        preview.path.clone()
    } else {
        format!("{} · {subtitle}", preview.path)
    };
    let block = pane(title, true).border_style(Style::new().fg(theme::MAUVE));

    // Line numbers only make sense for real text.
    let numbered = matches!(preview.body, PreviewBody::Text { .. });
    let gutter = lines.len().to_string().len();
    let visible = usize::from(popup.height.saturating_sub(2));
    let start = preview.scroll.min(lines.len().saturating_sub(visible));
    let shown: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, line)| {
            let mut spans = Vec::new();
            if numbered {
                spans.push(Span::styled(
                    format!("{:>gutter$} │ ", i + 1),
                    Style::new().fg(theme::SURFACE2),
                ));
            }
            spans.push(Span::raw(line.clone()));
            Line::from(spans)
        })
        .collect();
    let paragraph = Paragraph::new(shown)
        .style(Style::new().fg(theme::TEXT).bg(theme::BASE))
        .block(block);
    frame.render_widget(paragraph, popup);
}

/// The "new experiment" popup: a name and one line per param, the active one highlighted.
fn render_form(form: &ExperimentForm, frame: &mut Frame, area: Rect) {
    // name + fields + a line for the command preview or error; plus the border.
    let wanted = (form.fields.len() as u16 + 1) + 1 + 2;
    let width = (area.width * 80 / 100).max(30).min(area.width);
    let height = wanted.max(6).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);

    let title = if form.base.is_empty() {
        "New experiment".to_owned()
    } else {
        format!("New experiment · values from {}", form.base)
    };
    let block = pane(title, true).border_style(Style::new().fg(theme::MAUVE));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [rows_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    // Row 0 is the name; the others are the params. Scroll so the active row stays visible.
    let label_width = form
        .fields
        .iter()
        .map(|field| field.label.chars().count())
        .chain(["name".len()])
        .max()
        .unwrap_or(4)
        .min(34);
    let visible = usize::from(rows_area.height).max(1);
    let start = (form.active + 1).saturating_sub(visible);
    let row = |index: usize, label: &str, value: &str, original: Option<&str>| {
        let active = index == form.active;
        let changed = original.is_some_and(|original| original != value);
        let value_style = if changed {
            Style::new().fg(theme::PEACH).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::TEXT)
        };
        let mut spans = vec![
            Span::styled(
                if active { "▌ " } else { "  " },
                Style::new().fg(theme::MAUVE),
            ),
            Span::styled(format!("{label:<label_width$}  "), theme::label()),
            Span::styled(value.to_owned(), value_style),
        ];
        if active {
            spans.push(Span::styled("▏", Style::new().fg(theme::MAUVE)));
        }
        if changed {
            spans.push(Span::styled(
                format!("  (was {})", original.unwrap_or_default()),
                theme::muted(),
            ));
        }
        let line = Line::from(spans);
        if active {
            line.style(theme::selected())
        } else {
            line
        }
    };
    let mut lines = vec![row(0, "name", &form.name, None)];
    for (i, field) in form.fields.iter().enumerate() {
        lines.push(row(
            i + 1,
            &field.label,
            &field.value,
            Some(&field.original),
        ));
    }
    let shown: Vec<Line> = lines.into_iter().skip(start).take(visible).collect();
    frame.render_widget(Paragraph::new(shown), rows_area);

    let status = match (&form.error, form.command_preview()) {
        (Some(error), _) => Line::styled(format!("⚠ {error}"), Style::new().fg(theme::RED)),
        (None, Some(command)) => Line::styled(command, Style::new().fg(theme::TEAL)),
        (None, None) => Line::from(""),
    };
    frame.render_widget(Paragraph::new(status), status_area);
}

/// The list for hiding param columns, in the middle of the screen.
fn render_param_picker(
    picker: &ParamPicker,
    hidden: &BTreeSet<String>,
    frame: &mut Frame,
    area: Rect,
) {
    let width = (area.width * 60 / 100).max(40).min(area.width);
    let height = (picker.entries.len() as u16 + 2)
        .clamp(5, area.height.max(5))
        .min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let (mark, color) = match picker.shown(index, hidden) {
                Shown::All => ("[x]", theme::GREEN),
                Shown::Some => ("[-]", theme::YELLOW),
                Shown::None => ("[ ]", theme::OVERLAY0),
            };
            let count = if entry.members.len() > 1 {
                format!("  ({})", entry.members.len())
            } else {
                String::new()
            };
            Line::from(vec![
                Span::raw("  ".repeat(entry.depth)),
                Span::styled(mark, Style::new().fg(color)),
                Span::raw(format!(" {}", entry.text)),
                Span::styled(count, theme::muted()),
            ])
            .into()
        })
        .collect();
    let list = List::new(items)
        .block(pane("Params shown in the table", true).border_style(Style::new().fg(theme::PEACH)))
        .highlight_style(theme::selected())
        .highlight_symbol("▌ ");
    let mut state = ListState::default().with_selected(Some(picker.cursor));
    frame.render_stateful_widget(list, popup, &mut state);
}

/// A small yes/no popup in the middle of the screen.
fn render_confirm(confirm: &Confirm, frame: &mut Frame, area: Rect) {
    let width = (area.width * 60 / 100).max(40).min(area.width);
    // Count the lines after wrapping: the inner width is the popup minus its borders.
    let inner = usize::from(width.saturating_sub(2)).max(1);
    let wrapped: usize = confirm
        .lines
        .iter()
        .map(|line| line.chars().count().div_ceil(inner).max(1))
        .sum();
    // text + a blank line + the yes/no line + two border rows
    let height = (wrapped as u16 + 4).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let mut text: Vec<Line> = confirm
        .lines
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled(
            "y",
            Style::new().fg(theme::GREEN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" yes   ", theme::muted()),
        Span::styled(
            "n",
            Style::new().fg(theme::RED).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" no", theme::muted()),
    ]));
    let block = pane(confirm.title.as_str(), true).border_style(Style::new().fg(theme::PEACH));
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(block),
        popup,
    );
}

/// A one-line text prompt in the middle of the screen.
fn render_prompt(prompt: &Prompt, frame: &mut Frame, area: Rect) {
    let width = (area.width * 60 / 100).max(36).min(area.width);
    let height = 5.min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = pane(prompt.title.as_str(), true).border_style(Style::new().fg(theme::MAUVE));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [field, status] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    let line = Line::from(vec![
        Span::styled(format!("{}  ", prompt.label), theme::label()),
        Span::styled(prompt.value.clone(), Style::new().fg(theme::TEXT)),
        Span::styled("▏", Style::new().fg(theme::MAUVE)),
    ])
    .style(theme::selected());
    frame.render_widget(Paragraph::new(line), field);
    let message = match &prompt.error {
        Some(error) => Line::styled(format!("⚠ {error}"), Style::new().fg(theme::RED)),
        None => Line::styled(prompt.hint, theme::muted()),
    };
    frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), status);
}

fn render_detail(app: &App, frame: &mut Frame, area: Rect) {
    let title = app
        .selected_stage()
        .map_or("Detail".to_owned(), |s| s.name.clone());
    let detail = Paragraph::new(detail_lines(app))
        .block(pane(title, false))
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
        Span::styled(
            format!("{glyph} {label}"),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(status) = status {
        let why = status.reasons.iter().map(ToString::to_string).collect();
        lines.extend(section("why", why, Style::new().fg(theme::YELLOW)));
    }
    if let Some(last) = app.last_runs.get(&stage.name) {
        let (text, color) = outcome_label(&last.outcome);
        lines.push(Line::from(vec![
            label_span("last run"),
            Span::styled(text, Style::new().fg(color)),
            Span::styled(
                format!(" · {}", format_duration(last.duration)),
                theme::muted(),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.extend(section(
        "cmd",
        vec![stage.cmd.clone()],
        Style::new().fg(theme::TEAL),
    ));

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
        Style::new().fg(theme::MAUVE),
    ));

    lines.extend(section(
        "deps",
        stage.deps.clone(),
        kind_style(FileKind::Dep),
    ));
    let params = stage
        .params
        .iter()
        .flat_map(|param| param_lines(app, param))
        .collect();
    lines.extend(section("params", params, kind_style(FileKind::Param)));
    lines.extend(section(
        "outs",
        stage.outs.clone(),
        kind_style(FileKind::Out),
    ));
    lines.extend(section(
        "metrics",
        stage.metrics.clone(),
        kind_style(FileKind::Metric),
    ));

    if let Some(reason) = &stage.unsupported {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("{WARNING} {reason}"),
            Style::new().fg(theme::YELLOW),
        ));
    }
    lines
}

fn kind_style(kind: FileKind) -> Style {
    Style::new().fg(kind_color(kind))
}

fn label_span(label: &str) -> Span<'static> {
    Span::styled(format!("{label:<LABEL_WIDTH$}"), theme::label())
}

/// Glyph, text and colour for a status; `None` means the check has not finished yet.
pub(super) fn badge(status: Option<Status>) -> (&'static str, &'static str, Color) {
    match status {
        None => ("…", "checking", theme::OVERLAY0),
        Some(Status::UpToDate) => ("✓", "up to date", theme::GREEN),
        Some(Status::Stale) => ("●", "stale", theme::YELLOW),
        Some(Status::NeverRun) => ("○", "never run", theme::SUBTEXT0),
        Some(Status::Unverified) => ("?", "unverified", theme::MAUVE),
    }
}

/// One labelled block: the label sits on the first line, values are listed one per line below it.
/// An empty `values` produces no lines at all.
fn section(label: &str, values: Vec<String>, style: Style) -> Vec<Line<'static>> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let label = if i == 0 { label } else { "" };
            Line::from(vec![label_span(label), Span::styled(value, style)])
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
        files::Preview,
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
        // Row 0 is the header bar, row 1 the pane border; rows 2.. are list items.
        // No stale-check result has arrived yet, so each row shows the pending marker.
        assert!(rows[2].starts_with("│▌ … features"), "{rows:#?}");
        assert!(rows[3].starts_with("│  … train"), "{rows:#?}");
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
        assert!(rows[2].starts_with("│▌ ✓ prepare"), "{rows:#?}");
        assert!(rows[4].starts_with("│  ✓ train"), "{rows:#?}");
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
        let app = with_statuses(app_at(&fixture("downloader")));
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
        let app = crate::test_support::app_at(&crate::test_support::fixture("downloader"));
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
        let yaml = "stages:\n  each:\n    cmd: echo ${missing}\n";
        let (_dir, app) = app_with_files(&[("dvc.yaml", yaml)]);
        let text = screen(&app);
        assert!(text.contains("each ⚠"), "{text}");
        assert!(text.contains("unknown variable '${missing}'"), "{text}");
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
        run.log
            .push_str("dvc not found. Looked in:\n  here\n  there");
        run.log.finish();
        run.phase = RunPhase::Finished(RunOutcome::NotStarted("x".into()));
        app.run = Some(run);
        let text = screen(&app);
        assert!(
            text.contains("Looked in:") && text.contains("  here") && text.contains("  there"),
            "{text}"
        );
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

    #[test]
    fn header_names_the_app_and_marks_the_active_view() {
        let mut app = demo_app();
        let rows = render(&app, 100, 20);
        assert!(rows[0].contains("◆ dvcat"), "{rows:#?}");
        assert!(rows[0].contains("demo"), "{rows:#?}");
        assert!(rows[0].contains("Pipeline") && rows[0].contains("Experiments"));
        // The active tab is drawn on a filled background.
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let x = rows[0].find("Pipeline").unwrap() as u16;
        let cell = &terminal.backend().buffer()[(x, 0)];
        assert_eq!(cell.bg, theme::LAVENDER);
        app.view = View::Experiments;
        terminal.draw(|frame| view(&app, frame)).unwrap();
        assert_ne!(terminal.backend().buffer()[(x, 0)].bg, theme::LAVENDER);
    }

    #[test]
    fn the_whole_screen_gets_the_catppuccin_base_background() {
        let app = demo_app();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        // A corner nothing draws on, and the middle of a pane.
        assert_eq!(buffer[(59, 11)].bg, theme::BASE);
        assert_eq!(buffer[(40, 6)].bg, theme::BASE);
    }

    #[test]
    fn panes_have_rounded_corners_and_the_focused_one_is_lit() {
        let mut app = demo_app();
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].symbol(), "╭");
        assert_eq!(buffer[(0, 1)].fg, theme::LAVENDER, "stages pane is focused");
        // Find the files pane's top-left corner: the second `╭` in column 0.
        let files_top = (2..20).find(|&y| buffer[(0, y)].symbol() == "╭").unwrap();
        assert_eq!(buffer[(0, files_top)].fg, theme::SURFACE2);
        app.focus = Focus::Files;
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].fg, theme::SURFACE2);
        assert_eq!(buffer[(0, files_top)].fg, theme::LAVENDER);
    }

    #[test]
    fn files_pane_lists_the_selected_stages_files_with_their_kind() {
        let mut app = demo_app();
        app.selected = 2; // train
        let text = screen(&app);
        for expected in [
            "Files · train",
            "dep    src/train.py",
            "dep    data/features.csv",
            "param  params.yaml",
            "out    model.txt",
            "metric metrics.json",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn files_cursor_is_visible_only_when_the_pane_has_focus() {
        let mut app = demo_app();
        app.selected = 2;
        app.file_cursor = 1;
        assert!(!screen(&app).contains("▌ dep    data/features.csv"));
        app.focus = Focus::Files;
        let text = screen(&app);
        assert!(text.contains("▌ dep    data/features.csv"), "{text}");
    }

    #[test]
    fn a_stage_without_files_says_so() {
        let (_dir, app) = app_with_files(&[("dvc.yaml", "stages:\n  a:\n    cmd: x\n")]);
        assert!(screen(&app).contains("(no files)"));
    }

    fn app_with_preview(path: &str, body: PreviewBody) -> App {
        let mut app = demo_app();
        app.preview = Some(Preview::new(path.into(), body));
        app
    }

    fn text_body(text: &str) -> PreviewBody {
        PreviewBody::Text {
            text: text.into(),
            truncated: false,
        }
    }

    #[test]
    fn preview_popup_shows_numbered_lines_and_its_own_key_hints() {
        let app = app_with_preview("params.yaml", text_body("train:\n  lr: 0.01\n"));
        let rows = render(&app, 100, 24);
        let text = rows.join("\n");
        assert!(text.contains("params.yaml"), "{text}");
        assert!(text.contains("1 │ train:"), "{text}");
        assert!(text.contains("2 │   lr: 0.01"), "{text}");
        assert!(rows[23].contains("Esc close"), "{rows:#?}");
        assert!(!rows[23].contains("r run"), "pipeline hints are hidden");
    }

    #[test]
    fn preview_popup_covers_what_is_underneath() {
        let app = app_with_preview("a.txt", text_body("hello\n"));
        let rows = render(&app, 100, 24);
        // The popup is centred, so its middle rows must not still show the stage list or detail.
        assert!(!rows[10].contains("features"), "{rows:#?}");
        assert!(!rows[10].contains("python3"), "{rows:#?}");
    }

    #[test]
    fn preview_scrolls_with_the_scroll_offset() {
        let numbered: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let mut app = app_with_preview("big.txt", text_body(&numbered));
        assert!(
            screen_at(&app, 24).contains("line 1\n") || screen_at(&app, 24).contains("line 1 ")
        );
        app.preview.as_mut().unwrap().scroll = 60;
        let text = screen_at(&app, 24);
        assert!(text.contains("line 61"), "{text}");
        assert!(!text.contains("line 1 "), "{text}");
    }

    fn screen_at(app: &App, height: u16) -> String {
        render(app, 100, height).join("\n")
    }

    #[test]
    fn preview_explains_missing_binary_directory_and_unreadable_files() {
        let cases = [
            (PreviewBody::Missing, "does not exist (yet)"),
            (PreviewBody::Binary { bytes: 42 }, "binary file, 42 bytes"),
            (
                PreviewBody::Directory {
                    names: vec!["a.csv".into(), "sub/".into()],
                    truncated: false,
                },
                "directory · 2 entries",
            ),
            (
                PreviewBody::Unreadable("denied".into()),
                "cannot read: denied",
            ),
        ];
        for (body, expected) in cases {
            let text = screen_at(&app_with_preview("x", body), 24);
            assert!(text.contains(expected), "{expected:?} missing in:\n{text}");
        }
    }

    #[test]
    fn truncated_previews_say_so_in_the_title() {
        let body = PreviewBody::Text {
            text: "x\n".into(),
            truncated: true,
        };
        assert!(screen_at(&app_with_preview("big.csv", body), 24).contains("first 64 KiB"));
    }

    #[test]
    fn popup_survives_a_tiny_terminal() {
        let app = app_with_preview("a.txt", text_body("hello\n"));
        for (w, h) in [(10, 4), (30, 6), (1, 1)] {
            let _ = render(&app, w, h); // must not panic
        }
    }

    #[test]
    fn header_shows_the_running_stage() {
        let app = app_with_run("x\n", RunPhase::Running);
        assert!(render(&app, 100, 20)[0].contains("● running train"));
    }

    fn app_with_form() -> App {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        let table = crate::experiments::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        let row = table.rows.iter().find(|r| r.label == "master").unwrap();
        let mut app = demo_app();
        app.view = View::Experiments;
        app.form = Some(ExperimentForm::from_table(&table, Some(row)));
        app.experiments.data = Some(Ok(table));
        app
    }

    #[test]
    fn form_popup_lists_the_name_and_every_param_with_its_value() {
        let text = screen_at(&app_with_form(), 24);
        for expected in [
            "New experiment · values from master",
            "name",
            "params.yaml:train.lr",
            "0.01",
            "params.yaml:train.epochs",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
        // The popup replaces the table's key hints with its own.
        let last = render(&app_with_form(), 100, 24)[23].clone();
        assert!(
            last.contains("Enter run") && last.contains("Esc cancel"),
            "{last}"
        );
    }

    #[test]
    fn form_marks_changes_and_previews_the_command() {
        let mut app = app_with_form();
        let form = app.form.as_mut().unwrap();
        form.name = "lr-020".into();
        form.active = 1 + form
            .fields
            .iter()
            .position(|f| f.label == "params.yaml:train.lr")
            .unwrap();
        let field = &mut form.fields[form.active - 1];
        field.value = "0.2".into();
        let text = screen_at(&app, 24);
        assert!(text.contains("(was 0.01)"), "{text}");
        assert!(
            text.contains("0.2▏"),
            "cursor after the active value:\n{text}"
        );
        assert!(
            text.contains("dvc exp run --temp -n lr-020 -S params.yaml:train.lr=0.2"),
            "{text}"
        );
    }

    #[test]
    fn form_shows_the_error_instead_of_the_command() {
        let mut app = app_with_form();
        let form = app.form.as_mut().unwrap();
        form.error = Some("name may only contain letters".into());
        let text = screen_at(&app, 24);
        assert!(text.contains("⚠ name may only contain letters"), "{text}");
        assert!(!text.contains("dvc exp run"), "{text}");
    }

    #[test]
    fn form_survives_tiny_terminals_and_scrolls_to_the_active_field() {
        let mut app = app_with_form();
        for (w, h) in [(10, 4), (30, 6), (1, 1)] {
            let _ = render(&app, w, h); // must not panic
        }
        // With room for only two rows, the last field must still be visible when it is active.
        let last = app.form.as_ref().unwrap().fields.len();
        app.form.as_mut().unwrap().active = last;
        let final_label = app.form.as_ref().unwrap().fields[last - 1].label.clone();
        let text = screen_at(&app, 8);
        assert!(text.contains(&final_label), "{text}");
    }

    #[test]
    fn an_outside_run_is_named_in_the_header() {
        let mut app = demo_app();
        app.external = vec![crate::external::ExternalRun {
            pid: 1,
            command: "/v/bin/dvc repro pdf2md".into(),
            writes: Vec::new(),
            reads: Vec::new(),
        }];
        let text = render(&app, 140, 24).join("\n");
        assert!(text.contains("outside dvcat: dvc repro pdf2md"), "{text}");
        assert!(text.contains("X stops it"), "{text}");
    }

    #[test]
    fn confirm_popup_shows_the_question_and_its_own_key_hints() {
        let mut app = demo_app();
        app.confirm = Some(Confirm {
            title: "Remove lr-005?".into(),
            lines: vec!["Deletes the experiment.".into()],
            label: "remove lr-005".into(),
            args: vec![],
            kill: vec![],
        });
        let rows = render(&app, 100, 24);
        let text = rows.join("\n");
        assert!(text.contains("Remove lr-005?"), "{text}");
        assert!(text.contains("Deletes the experiment."), "{text}");
        assert!(
            rows[23].contains("y/Enter yes") && rows[23].contains("n/Esc no"),
            "{rows:#?}"
        );
        for (w, h) in [(10, 4), (1, 1)] {
            let _ = render(&app, w, h); // must not panic
        }
    }

    #[test]
    fn expanded_stages_are_listed_by_their_dvc_names_in_dependency_order() {
        let app = with_statuses(app_at(&fixture("matrix")));
        let rows = render(&app, 100, 20);
        let text = rows.join("\n");
        for name in ["each@us", "each@eu", "grid@lin-1", "grid@win-2", "collect"] {
            assert!(text.contains(name), "missing {name}:\n{text}");
        }
        let line_of = |name: &str| rows.iter().position(|r| r.contains(name)).unwrap();
        assert!(line_of("each@eu") < line_of("collect"));
        assert!(!text.contains("⚠"), "nothing should be flagged:\n{text}");
    }

    #[test]
    fn detail_pane_names_the_missing_output() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::copy_dir(&fixture("demo"), dir.path());
        std::fs::remove_file(dir.path().join("model.txt")).unwrap();
        let mut app = with_statuses(app_at(dir.path()));
        app.selected = 2; // train
        let text = screen(&app);
        assert!(text.contains("status   ● stale"), "{text}");
        assert!(
            text.contains("why      missing output: model.txt"),
            "{text}"
        );
        let rows = render(&app, 100, 20);
        assert!(
            rows[2].starts_with("│  ✓ prepare") && rows[4].starts_with("│▌ ● train"),
            "{rows:#?}"
        );
    }

    #[test]
    fn branch_prompt_shows_the_name_a_cursor_and_its_own_hints() {
        let mut app = demo_app();
        app.prompt = Some(crate::prompt::Prompt::branch("lr-005"));
        let rows = render(&app, 100, 24);
        let text = rows.join("\n");
        assert!(text.contains("Branch from lr-005"), "{text}");
        assert!(text.contains("branch name  lr-005-branch▏"), "{text}");
        assert!(text.contains("not checked out"), "{text}");
        assert!(
            rows[23].contains("Enter ok") && rows[23].contains("Esc cancel"),
            "{rows:#?}"
        );
        for (w, h) in [(10, 4), (1, 1)] {
            let _ = render(&app, w, h);
        }
    }

    #[test]
    fn branch_prompt_shows_validation_errors() {
        let mut app = demo_app();
        let mut prompt = crate::prompt::Prompt::branch("a");
        prompt.error = Some("'~' is not allowed in a branch name".into());
        app.prompt = Some(prompt);
        let text = screen_at(&app, 24);
        assert!(text.contains("⚠ '~' is not allowed"), "{text}");
        assert!(!text.contains("creates the branch"), "{text}");
    }

    // ---- graph view ----

    fn graph_app(app: App) -> App {
        let mut app = with_statuses(app);
        app.view = View::Dag;
        app
    }

    #[test]
    fn graph_draws_boxes_with_status_glyphs_and_arrows() {
        let app = graph_app(demo_app());
        let rows = render(&app, 100, 20);
        let text = rows.join("\n");
        assert!(text.contains("◆ dvcat") && text.contains("Graph"), "{text}");
        for name in ["prepare", "features", "train"] {
            assert!(
                text.contains(&format!("✓ {name}")),
                "missing {name}:\n{text}"
            );
        }
        assert_eq!(text.matches('▶').count(), 2, "{text}");
        assert!(text.contains("╭") && text.contains("╰"), "{text}");
        // prepare is left of features is left of train
        let line = rows.iter().find(|r| r.contains("✓ prepare")).unwrap();
        assert!(
            line.find("prepare") < line.find("features")
                && line.find("features") < line.find("train"),
            "{line}"
        );
    }

    #[test]
    fn the_selected_box_is_highlighted_and_described_below() {
        let mut app = graph_app(demo_app());
        app.selected = 2;
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let rows: Vec<String> = (0..20)
            .map(|y| {
                (0..100)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect();
        let y = rows.iter().position(|r| r.contains("✓ train")).unwrap() as u16;
        let x = rows[y as usize].find("train").unwrap();
        let x = rows[y as usize][..x].chars().count() as u16;
        assert_eq!(terminal.backend().buffer()[(x, y)].bg, theme::SURFACE0);
        let other = rows[y as usize].find("prepare").unwrap();
        let other = rows[y as usize][..other].chars().count() as u16;
        assert_ne!(terminal.backend().buffer()[(other, y)].bg, theme::SURFACE0);
        assert!(
            rows.join("\n").contains("train · ✓ up to date"),
            "{}",
            rows.join("\n")
        );
    }

    #[test]
    fn stale_stages_show_their_colour_and_first_reason() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::copy_dir(&fixture("demo"), dir.path());
        std::fs::remove_file(dir.path().join("model.txt")).unwrap();
        let mut app = graph_app(app_at(dir.path()));
        app.selected = 2;
        let text = render(&app, 100, 20).join("\n");
        assert!(text.contains("● train"), "{text}");
        assert!(
            text.contains("train · ● stale · missing output: model.txt"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_project_says_so_in_the_graph_view() {
        let (_dir, app) = app_with_files(&[("dvc.yaml", "stages: {}\n")]);
        let app = graph_app(app);
        assert!(
            render(&app, 100, 20)
                .join("\n")
                .contains("(no stages in dvc.yaml)")
        );
    }

    #[test]
    fn a_graph_wider_than_the_screen_scrolls_to_the_selected_stage() {
        let mut yaml = String::from("stages:\n");
        for i in 0..12 {
            let dep = if i == 0 {
                String::new()
            } else {
                format!("    deps: [o{}.txt]\n", i - 1)
            };
            yaml.push_str(&format!(
                "  stage_number_{i}:\n    cmd: x\n{dep}    outs: [o{i}.txt]\n"
            ));
        }
        let (_dir, app) = app_with_files(&[("dvc.yaml", &yaml)]);
        let mut app = graph_app(app);
        let first = render(&app, 70, 14).join("\n");
        assert!(
            first.contains("stage_number_0") && !first.contains("stage_number_11"),
            "{first}"
        );
        app.selected = 11;
        let last = render(&app, 70, 14).join("\n");
        assert!(
            last.contains("stage_number_11") && !last.contains("stage_number_0 "),
            "{last}"
        );
    }

    #[test]
    fn the_graph_view_survives_tiny_terminals() {
        let app = graph_app(demo_app());
        for (w, h) in [(10, 4), (30, 6), (1, 1), (60, 3)] {
            let _ = render(&app, w, h);
        }
    }

    #[test]
    fn graph_footer_lists_its_keys() {
        let app = graph_app(demo_app());
        let rows = render(&app, 120, 20);
        assert!(
            rows[19].contains("h/l upstream/downstream") && rows[19].contains("Enter open stage"),
            "{rows:#?}"
        );
    }

    #[test]
    fn the_graph_info_line_does_not_repeat_never_run() {
        let (_dir, app) = app_with_files(&[("dvc.yaml", "stages:\n  a:\n    cmd: x\n")]);
        let app = graph_app(with_statuses(app));
        let text = render(&app, 100, 20).join("\n");
        assert!(text.contains("a · ○ never run"), "{text}");
        assert!(!text.contains("never run · never run"), "{text}");
    }

    #[test]
    fn graph_boxes_take_their_border_colour_from_the_stage_status() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_support::copy_dir(&fixture("demo"), dir.path());
        std::fs::remove_file(dir.path().join("model.txt")).unwrap();
        let mut app = graph_app(app_at(dir.path()));
        app.selected = 0; // the cursor is elsewhere, so train's border shows its status colour
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| view(&app, frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let find = |name: &str| {
            (0..20u16)
                .find_map(|y| {
                    let row: String = (0..100u16)
                        .map(|x| buffer[(x, y)].symbol().to_owned())
                        .collect();
                    row.find(name)
                        .map(|byte| (row[..byte].chars().count() as u16, y))
                })
                .unwrap()
        };
        let (x, y) = find("train");
        assert_eq!(
            buffer[(x - 4, y - 1)].fg,
            theme::YELLOW,
            "stale = yellow border"
        );
        let (x, y) = find("features");
        assert_eq!(
            buffer[(x - 4, y - 1)].fg,
            theme::GREEN,
            "up to date = green border"
        );
    }

    #[test]
    fn durations_read_naturally_at_every_scale() {
        use std::time::Duration as D;
        let cases = [
            (D::from_millis(0), "0ms"),
            (D::from_millis(850), "850ms"),
            (D::from_millis(4_200), "4.2s"),
            (D::from_secs(59), "59.0s"),
            (D::from_secs(125), "2m 05s"),
            (D::from_secs(3_720), "1h 02m"),
        ];
        for (duration, expected) in cases {
            assert_eq!(format_duration(duration), expected);
        }
    }

    #[test]
    fn the_log_title_shows_the_time_a_run_took() {
        let mut app = demo_app();
        let mut run = Run::new("train".into(), false);
        run.phase = RunPhase::Finished(RunOutcome::Succeeded);
        app.run = Some(run);
        let text = screen(&app);
        let title = text.lines().find(|l| l.contains("dvc repro")).unwrap();
        assert!(title.contains("✓ succeeded · "), "{title}");
        assert!(title.contains("ms") || title.contains('s'), "{title}");
    }

    #[test]
    fn the_detail_pane_remembers_how_the_last_run_of_a_stage_ended() {
        use crate::app::LastRun;
        let mut app = demo_app();
        assert!(!screen(&app).contains("last run"));
        app.last_runs.insert(
            "prepare".into(),
            LastRun {
                outcome: RunOutcome::Failed { code: Some(2) },
                duration: std::time::Duration::from_millis(4_200),
            },
        );
        let text = screen(&app);
        assert!(text.contains("last run ✗ failed (exit 2) · 4.2s"), "{text}");
        app.selected = 1; // another stage has no last run
        assert!(!screen(&app).contains("last run"));
    }

    #[test]
    fn the_stage_list_shows_the_last_run_time_right_aligned_only_for_stages_that_have_one() {
        use crate::app::LastRun;
        let mut app = demo_app();
        app.last_runs.insert(
            "features".into(),
            LastRun {
                outcome: RunOutcome::Succeeded,
                duration: std::time::Duration::from_millis(3_200),
            },
        );
        let rows = render(&app, 100, 20);
        // The stage list is the left 35 columns of the screen.
        let left = |name: &str| -> String {
            rows.iter()
                .find(|r| r.contains(name))
                .unwrap()
                .chars()
                .take(35)
                .collect()
        };
        let features = left("features");
        assert!(
            features.trim_end_matches('│').trim_end().ends_with("3.2s"),
            "{features:?}"
        );
        assert!(features.find("features").unwrap() < features.find("3.2s").unwrap());
        assert!(
            !left("prepare").contains("ms") && !left("prepare").contains("s │"),
            "{:?}",
            left("prepare")
        );
        assert_eq!(
            rows.join("\n").matches("3.2s").count(),
            1,
            "only one time on screen"
        );
    }

    #[test]
    fn a_pane_too_narrow_for_the_time_drops_it_instead_of_overflowing() {
        use crate::app::LastRun;
        let mut app = demo_app();
        app.last_runs.insert(
            "features".into(),
            LastRun {
                outcome: RunOutcome::Succeeded,
                duration: std::time::Duration::from_secs(125),
            },
        );
        for width in [12, 20, 30] {
            let _ = render(&app, width, 12); // never panics
        }
        let narrow = render(&app, 24, 12).join("\n");
        assert!(
            !narrow.contains("2m 05s") || narrow.contains("features"),
            "{narrow}"
        );
    }

    #[test]
    fn the_header_numbers_the_views_and_the_footer_mentions_the_graph_key() {
        let app = demo_app();
        let rows = render(&app, 120, 20);
        assert!(
            rows[0].contains("1 Pipeline")
                && rows[0].contains("2 Graph (g)")
                && rows[0].contains("3 Experiments (e)"),
            "{}",
            rows[0]
        );
        assert!(rows[19].contains("g graph"), "{}", rows[19]);
        let mut files = demo_app();
        files.focus = Focus::Files;
        assert!(render(&files, 120, 20)[19].contains("g graph"));
    }

    #[test]
    fn plots_view_lists_plots_draws_the_selected_one_and_explains_a_broken_one() {
        let (_dir, mut app) = app_with_files(&[
            (
                "dvc.yaml",
                "stages:\n  a:\n    cmd: echo\nplots:\n  - acc.csv:\n      x: epoch\n      title: Accuracy\n  - gone.csv\n",
            ),
            ("acc.csv", "epoch,acc\n1,0.5\n2,0.7\n3,0.9\n"),
        ]);
        app.update(crate::app::Event::Key(
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char('p'),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
        ));
        let text = render(&app, 100, 24).join("\n");
        assert!(
            text.contains("Plots (2)") && text.contains("acc.csv") && text.contains("⚠ gone.csv"),
            "{text}"
        );
        assert!(
            text.contains("Accuracy") && text.contains("━ acc"),
            "{text}"
        );
        app.plots.selected = 1;
        let text = render(&app, 100, 24).join("\n");
        assert!(text.contains("cannot read gone.csv"), "{text}");
    }

    #[test]
    fn plots_view_without_plots_shows_how_to_add_them() {
        let mut app = demo_app();
        app.view = View::Plots;
        app.plots.data = Some(Ok(Vec::new()));
        assert!(render(&app, 100, 24).join("\n").contains("plots:"));
    }

    #[test]
    fn plots_view_shows_the_definition_and_the_new_plot_form() {
        let (_dir, mut app) = app_with_files(&[
            (
                "dvc.yaml",
                "stages:\n  a:\n    cmd: echo\nplots:\n  - acc.csv:\n      x: epoch\n",
            ),
            ("acc.csv", "epoch,acc\n1,0.5\n2,0.7\n"),
        ]);
        let press = |app: &mut App, c: char| {
            app.update(crate::app::Event::Key(
                ratatui::crossterm::event::KeyEvent::new(
                    ratatui::crossterm::event::KeyCode::Char(c),
                    ratatui::crossterm::event::KeyModifiers::NONE,
                ),
            ));
        };
        press(&mut app, 'p');
        press(&mut app, 'd');
        let text = render(&app, 100, 28).join("\n");
        assert!(
            text.contains("Definition in dvc.yaml") && text.contains("x: epoch"),
            "{text}"
        );
        press(&mut app, 'n');
        for c in "acc.csv".chars() {
            press(&mut app, c);
        }
        let text = render(&app, 100, 28).join("\n");
        assert!(
            text.contains("New plot") && text.contains("columns: epoch, acc"),
            "{text}"
        );
    }

    #[test]
    fn the_stage_form_shows_fields_toggles_and_the_hint_of_the_active_one() {
        let mut app = demo_app();
        app.stage_form = Some(crate::stage_form::StageForm::default());
        let text = render(&app, 110, 28).join("\n");
        assert!(
            text.contains("New stage") && text.contains("cmd") && text.contains("metrics"),
            "{text}"
        );
        assert!(
            text.contains("[x] cache") && text.contains("[ ] persist"),
            "{text}"
        );
        assert!(text.contains("stage name"), "{text}");
        app.stage_form.as_mut().unwrap().active = 7;
        app.stage_form.as_mut().unwrap().flags[1] = true;
        let text = render(&app, 110, 28).join("\n");
        assert!(
            text.contains("[x] persist") && text.contains("do not delete the outputs"),
            "{text}"
        );
    }
}
