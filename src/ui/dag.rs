//! The graph view: boxes and lines from [`crate::dag::Layout`], coloured by stage status.

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    widgets::Paragraph,
};

use crate::{
    app::App,
    dag::{self, Placed},
    theme,
};

pub fn render(app: &App, frame: &mut Frame, area: Rect) {
    let block = super::pane("Graph", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = &app.dag;
    if layout.nodes.is_empty() {
        frame.render_widget(
            Paragraph::new("(no stages in dvc.yaml)").style(theme::muted()),
            inner,
        );
        return;
    }
    let [canvas, info] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let cursor = app.graph.order.get(app.selected).copied();
    let (origin_x, origin_y) = match cursor {
        Some(stage) => origin(
            &layout.nodes[stage],
            (layout.width, layout.height),
            (canvas.width, canvas.height),
        ),
        None => (0, 0),
    };
    let mut painter = Painter {
        buffer: frame.buffer_mut(),
        canvas,
        origin: (origin_x, origin_y),
    };

    // Lines first, boxes on top.
    for (y, row) in layout.lines.iter().enumerate() {
        for (x, &c) in row.iter().enumerate() {
            if c == ' ' {
                continue;
            }
            let style = if c == '▶' {
                Style::new().fg(theme::OVERLAY1)
            } else {
                Style::new().fg(theme::SURFACE2)
            };
            painter.put(x, y, c, style);
        }
    }
    for (stage, node) in layout.nodes.iter().enumerate() {
        draw_box(app, &mut painter, stage, node, cursor == Some(stage));
    }

    frame.render_widget(Paragraph::new(info_line(app)).style(theme::muted()), info);
}

/// Top-left corner of the visible part of the drawing: it shows everything when it fits and
/// otherwise centres the cursor's box, without scrolling past the edges.
fn origin(node: &Placed, total: (u16, u16), view: (u16, u16)) -> (u16, u16) {
    let centre = |position: u16, size: u16, total: u16, view: u16| -> u16 {
        if total <= view {
            0
        } else {
            (position + size / 2)
                .saturating_sub(view / 2)
                .min(total - view)
        }
    };
    (
        centre(node.x, node.w, total.0, view.0),
        centre(node.y, Placed::HEIGHT, total.1, view.1),
    )
}

/// Writes cells of the drawing into the screen buffer, clipped to the canvas area.
struct Painter<'a> {
    buffer: &'a mut Buffer,
    canvas: Rect,
    origin: (u16, u16),
}

impl Painter<'_> {
    fn put(&mut self, x: usize, y: usize, c: char, style: Style) {
        let (Some(sx), Some(sy)) = (
            x.checked_sub(usize::from(self.origin.0)),
            y.checked_sub(usize::from(self.origin.1)),
        ) else {
            return;
        };
        if sx >= usize::from(self.canvas.width) || sy >= usize::from(self.canvas.height) {
            return;
        }
        let cell = &mut self.buffer[(self.canvas.x + sx as u16, self.canvas.y + sy as u16)];
        cell.set_char(c).set_style(style);
    }

    fn text(&mut self, x: usize, y: usize, text: &str, style: Style) {
        for (offset, c) in text.chars().enumerate() {
            self.put(x + offset, y, c, style);
        }
    }
}

fn draw_box(app: &App, painter: &mut Painter, stage: usize, node: &Placed, selected: bool) {
    let position = app.graph.order.iter().position(|&s| s == stage);
    let status = position.and_then(|p| app.status_at(p)).map(|s| s.status());
    let (glyph, _, colour) = super::badge(status);

    let (x, y, w) = (
        usize::from(node.x),
        usize::from(node.y),
        usize::from(node.w),
    );
    let fill = if selected {
        Style::new().bg(theme::SURFACE0)
    } else {
        Style::new()
    };
    let border = fill.fg(if selected { theme::LAVENDER } else { colour });
    let inner = w - 2;

    painter.text(x, y, &format!("╭{}╮", "─".repeat(inner)), border);
    painter.text(x, y + 2, &format!("╰{}╯", "─".repeat(inner)), border);
    painter.put(x, y + 1, '│', border);
    painter.put(x + w - 1, y + 1, '│', border);

    let name = dag::display_name(&app.project.stages[stage].name);
    let padding = inner.saturating_sub(4 + name.chars().count());
    painter.put(x + 1, y + 1, ' ', fill);
    painter.put(
        x + 2,
        y + 1,
        glyph.chars().next().unwrap_or(' '),
        fill.fg(colour),
    );
    painter.put(x + 3, y + 1, ' ', fill);
    let name_style = if selected {
        fill.fg(theme::TEXT).add_modifier(Modifier::BOLD)
    } else {
        fill.fg(theme::TEXT)
    };
    painter.text(x + 4, y + 1, &name, name_style);
    painter.text(
        x + 4 + name.chars().count(),
        y + 1,
        &" ".repeat(padding + 1),
        fill,
    );
}

/// `train · ● stale · missing output: model.txt` for the stage under the cursor.
fn info_line(app: &App) -> String {
    let Some(stage) = app.selected_stage() else {
        return String::new();
    };
    let status = app.status_at(app.selected);
    let (glyph, label, _) = super::badge(status.map(|s| s.status()));
    let mut text = format!("{} · {glyph} {label}", stage.name);
    // "never run · never run" says nothing new, so skip a reason that repeats the status.
    if let Some(reason) = status
        .and_then(|s| s.reasons.first())
        .map(ToString::to_string)
        .filter(|reason| reason != label)
    {
        text.push_str(&format!(" · {reason}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(x: u16, y: u16, w: u16) -> Placed {
        Placed { x, y, w, layer: 0 }
    }

    #[test]
    fn a_drawing_that_fits_is_not_scrolled() {
        assert_eq!(origin(&node(30, 4, 10), (60, 12), (80, 20)), (0, 0));
    }

    #[test]
    fn the_cursor_is_centred_when_the_drawing_is_larger() {
        // 200 wide in a 60-wide view: centre of the box is x=105, so the view starts at 75.
        assert_eq!(origin(&node(100, 0, 10), (200, 8), (60, 20)).0, 75);
    }

    #[test]
    fn scrolling_stops_at_the_edges() {
        assert_eq!(origin(&node(0, 0, 10), (200, 40), (60, 10)), (0, 0));
        assert_eq!(origin(&node(190, 36, 10), (200, 40), (60, 10)), (140, 30));
    }
}
