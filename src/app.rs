//! Application state and the one place that changes it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Everything that can happen to the app arrives as an `Event`.
/// Later groups add variants here (run output, experiment results, ...).
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
}

#[derive(Debug)]
pub struct App {
    /// Placeholder list; replaced by real stages in group 3.
    pub items: Vec<String>,
    pub selected: usize,
    pub should_quit: bool,
}

impl App {
    pub fn new(items: Vec<String>) -> Self {
        Self {
            items,
            selected: 0,
            should_quit: false,
        }
    }

    /// The only function that mutates the app (`&mut self`).
    pub fn update(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Raw mode turns off the terminal's own Ctrl+C handling, so we do it ourselves.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection_up(),
            _ => {}
        }
    }

    fn move_selection_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    fn move_selection_up(&mut self) {
        // `saturating_sub` stops at 0 instead of underflowing (usize can't be negative).
        self.selected = self.selected.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(vec!["a".into(), "b".into(), "c".into()])
    }

    fn press(app: &mut App, code: KeyCode) {
        app.update(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn q_quits() {
        let mut app = app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app();
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut app = app();
        press(&mut app, KeyCode::Char('c'));
        assert!(!app.should_quit);
    }

    #[test]
    fn j_and_down_move_down() {
        let mut app = app();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn selection_stops_at_last_item() {
        let mut app = app();
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn k_and_up_move_up_and_stop_at_zero() {
        let mut app = app();
        app.selected = 2;
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn movement_on_empty_list_is_safe() {
        let mut app = App::new(Vec::new());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn unknown_key_changes_nothing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.selected, 0);
        assert!(!app.should_quit);
    }
}
