//! The "new stage" form in the middle of the screen: text fields, then the on/off options.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use crate::{
    stage_form::{StageForm, TEXT_FIELDS, TEXTS, TOGGLES},
    theme,
};

const MARK_ON: &str = "[x]";
const MARK_OFF: &str = "[ ]";

pub fn render(form: &StageForm, frame: &mut Frame, area: Rect) {
    let count = StageForm::field_count() as u16;
    let width = (area.width * 85 / 100).max(40).min(area.width);
    // Fields, a spacer before the options, two status lines and the border.
    let height = (count + 1 + 2 + 2).min(area.height);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = super::pane("New stage · added to dvc.yaml", true)
        .border_style(Style::new().fg(theme::MAUVE));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [rows_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);

    let label_width = TEXT_FIELDS
        .iter()
        .map(|(label, _)| label.len())
        .chain(
            TOGGLES
                .iter()
                .map(|(label, _)| label.len() + MARK_ON.len() + 1),
        )
        .max()
        .unwrap_or(0);
    let marker = |active: bool| {
        Span::styled(
            if active { "▌ " } else { "  " },
            Style::new().fg(theme::MAUVE),
        )
    };
    let highlight = |line: Line<'static>, active: bool| {
        if active {
            line.style(theme::selected())
        } else {
            line
        }
    };

    let mut lines: Vec<Line> = Vec::new();
    for (index, (label, _)) in TEXT_FIELDS.iter().enumerate() {
        let active = index == form.active;
        let mut spans = vec![
            marker(active),
            Span::styled(format!("{label:<label_width$}  "), theme::label()),
            Span::styled(form.texts[index].clone(), Style::new().fg(theme::TEXT)),
        ];
        if active {
            spans.push(Span::styled("▏", Style::new().fg(theme::MAUVE)));
        }
        lines.push(highlight(Line::from(spans), active));
    }
    lines.push(Line::from(""));
    for (index, (label, _)) in TOGGLES.iter().enumerate() {
        let active = TEXTS + index == form.active;
        let on = form.flags[index];
        let (mark, color) = if on {
            (MARK_ON, theme::GREEN)
        } else {
            (MARK_OFF, theme::OVERLAY0)
        };
        let spans = vec![
            marker(active),
            Span::styled(mark, Style::new().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {label}"), Style::new().fg(theme::TEXT)),
        ];
        lines.push(highlight(Line::from(spans), active));
    }
    frame.render_widget(Paragraph::new(lines), rows_area);

    let hint = if form.active < TEXTS {
        TEXT_FIELDS[form.active].1
    } else {
        TOGGLES[form.active - TEXTS].1
    };
    let status = match &form.error {
        Some(error) => Line::styled(format!("⚠ {error}"), Style::new().fg(theme::RED)),
        None => Line::styled(hint, theme::muted()),
    };
    frame.render_widget(Paragraph::new(vec![status]), status_area);
}
