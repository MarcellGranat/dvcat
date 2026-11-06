//! Drawing. Everything here takes `&App`: it can read state but never change it.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Style, Stylize},
    widgets::{Block, List, ListState, Paragraph},
};

use crate::app::App;

const HELP: &str = " j/k move   q quit ";

pub fn view(app: &App, frame: &mut Frame) {
    // Split the screen: a big area on top, one line for help at the bottom.
    let [main, help] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    // Split the main area into a list (left) and a detail pane (right).
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main);

    let list = List::new(app.items.iter().map(String::as_str))
        .block(Block::bordered().title(" Stages "))
        .highlight_style(Style::new().reversed())
        .highlight_symbol("▶ ");
    // ListState is a small scratch value rebuilt each frame from our own state.
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list, left, &mut state);

    let detail = app
        .items
        .get(app.selected)
        .map_or("(nothing selected)", String::as_str);
    frame.render_widget(
        Paragraph::new(detail).block(Block::bordered().title(" Detail ")),
        right,
    );

    frame.render_widget(Paragraph::new(HELP).dim(), help);
}
