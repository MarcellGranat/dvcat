//! The command palette: every action with its key, searchable, and runnable from the list.
//!
//! `?`, `:` or `Ctrl+P` open it. It is also the full key help: the footer only has room for the
//! most useful keys, this list has them all.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::form::edit_text;

/// Where an action works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Everywhere,
    Pipeline,
    Files,
    Experiments,
    Dag,
    Plots,
}

/// What choosing an entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Do {
    /// Press this key, as if the user had.
    Key(KeyCode),
    /// Show the notices of this session.
    Messages,
    /// Switch to the next colour theme.
    Theme,
    /// Information only (a key that needs context).
    Nothing,
}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub place: Where,
    pub keys: &'static str,
    pub what: &'static str,
    pub action: Do,
}

const fn key(place: Where, keys: &'static str, c: char, what: &'static str) -> Entry {
    Entry {
        place,
        keys,
        what,
        action: Do::Key(KeyCode::Char(c)),
    }
}

const fn code(place: Where, keys: &'static str, k: KeyCode, what: &'static str) -> Entry {
    Entry {
        place,
        keys,
        what,
        action: Do::Key(k),
    }
}

const fn info(place: Where, keys: &'static str, what: &'static str) -> Entry {
    Entry {
        place,
        keys,
        what,
        action: Do::Nothing,
    }
}

use Where::{Dag, Everywhere, Experiments, Files, Pipeline, Plots};

pub const ENTRIES: &[Entry] = &[
    // Everywhere
    key(Everywhere, "1", '1', "pipeline view"),
    key(Everywhere, "2", '2', "graph view"),
    key(
        Everywhere,
        "e",
        'e',
        "experiments view (again: back to the pipeline)",
    ),
    key(Everywhere, "4", '4', "plots view"),
    key(Everywhere, "P", 'P', "dvc pull (choose the remote)"),
    key(Everywhere, "U", 'U', "dvc push (choose the remote)"),
    key(Everywhere, "x", 'x', "cancel the running job"),
    key(Everywhere, "X", 'X', "stop dvc runs started elsewhere"),
    Entry {
        place: Everywhere,
        keys: ":messages",
        what: "show the notices of this session",
        action: Do::Messages,
    },
    Entry {
        place: Everywhere,
        keys: ":theme",
        what: "switch the colour theme (mocha, latte, colour-blind)",
        action: Do::Theme,
    },
    key(Everywhere, "q", 'q', "quit"),
    // Pipeline
    key(Pipeline, "j/k", 'j', "next stage"),
    code(
        Pipeline,
        "Tab",
        KeyCode::Tab,
        "go to the files of the stage",
    ),
    key(
        Pipeline,
        "z",
        'z',
        "fold or open a group of stages (name@key)",
    ),
    key(Pipeline, "r", 'r', "run the selected stage"),
    key(
        Pipeline,
        "R",
        'R',
        "run the selected stage with its upstream",
    ),
    key(Pipeline, "n", 'n', "new stage"),
    key(
        Pipeline,
        "o",
        'o',
        "open the stage's script or params file in $EDITOR",
    ),
    code(Pipeline, "PgUp/PgDn", KeyCode::PageUp, "scroll the log"),
    key(Pipeline, "g", 'g', "graph view"),
    // Files
    code(Files, "Enter", KeyCode::Enter, "preview the file"),
    key(Files, "o", 'o', "open the file in $EDITOR"),
    key(Files, "r", 'r', "run the stage"),
    // Graph
    info(Dag, "h/j/k/l", "move between stages"),
    key(Dag, "r", 'r', "run the stage"),
    // Plots
    info(Plots, "j/k", "choose a plot"),
    key(Plots, "n", 'n', "new plot"),
    // Experiments
    info(Experiments, "j/k h/l", "move the cursor"),
    key(
        Experiments,
        "Space",
        ' ',
        "mark the row under the cursor (Space)",
    ),
    code(
        Experiments,
        "Enter",
        KeyCode::Enter,
        "focus: one experiment with all its fields",
    ),
    key(
        Experiments,
        ",",
        ',',
        "mark every row with the cursor cell's value",
    ),
    key(
        Experiments,
        "z",
        'z',
        "show only the marked rows (again: all)",
    ),
    key(
        Experiments,
        "T",
        'T',
        "mark the best N rows of the cursor's metric",
    ),
    key(Experiments, "=", '=', "compare the two marked rows"),
    key(Experiments, "s", 's', "sort by the cursor's column"),
    code(
        Experiments,
        "Tab",
        KeyCode::Tab,
        "params / metrics / all columns",
    ),
    key(Experiments, "/", '/', "filter the columns by words"),
    key(Experiments, "u", 'u', "show or hide the unchanged columns"),
    key(Experiments, "{ }", '}', "jump to the next column group"),
    key(
        Experiments,
        "f",
        'f',
        "pin or unpin the cursor's column (stays visible)",
    ),
    key(
        Experiments,
        "F",
        'F',
        "fold or unfold the by_fold metrics into one mean ±spread column",
    ),
    key(
        Experiments,
        "i",
        'i',
        "show or hide the commits no experiment ran on",
    ),
    key(Experiments, "-", '-', "hide the cursor's column"),
    key(Experiments, "+", '+', "show the hidden columns again"),
    key(Experiments, "v", 'v', "choose the param columns"),
    key(
        Experiments,
        "m",
        'm',
        "metrics as differences from the baseline",
    ),
    key(Experiments, "t", 't', "transpose the table"),
    key(Experiments, "p", 'p', "chart under the table"),
    key(Experiments, "[ ]", ']', "next metric in the chart"),
    key(
        Experiments,
        "B",
        'B',
        "metric direction: auto, lower or higher is better",
    ),
    key(Experiments, "y", 'y', "copy the cursor cell"),
    key(Experiments, "Y", 'Y', "copy the row as JSON"),
    key(
        Experiments,
        "E",
        'E',
        "export the table as it is shown to dvcat-experiments.csv",
    ),
    key(
        Experiments,
        "o",
        'o',
        "open the params file of the cursor's column in $EDITOR",
    ),
    key(Experiments, "n", 'n', "new experiment"),
    key(
        Experiments,
        "a",
        'a',
        "apply the experiment to the workspace",
    ),
    key(Experiments, "b", 'b', "branch from the experiment"),
    key(Experiments, "c", 'c', "comment on the experiment"),
    key(Experiments, "d", 'd', "remove the experiment"),
    code(Experiments, "F5", KeyCode::F(5), "refresh"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub cursor: usize,
    pub place: Where,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    Continue,
    Close,
    Run(Entry),
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.keys == other.keys && self.what == other.what
    }
}

impl Palette {
    pub fn new(place: Where) -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            place,
        }
    }

    /// The entries that work here and contain every word of the query, this view's first.
    pub fn matches(&self) -> Vec<&'static Entry> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut found: Vec<&Entry> = ENTRIES
            .iter()
            .filter(|entry| entry.place == self.place || entry.place == Where::Everywhere)
            .filter(|entry| {
                let text = format!("{} {}", entry.keys, entry.what).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .collect();
        found.sort_by_key(|entry| entry.place == Where::Everywhere);
        found
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaletteAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.matches().len();
        match key.code {
            KeyCode::Esc => return PaletteAction::Close,
            KeyCode::Enter => {
                return match self.matches().get(self.cursor) {
                    Some(entry) if entry.action != Do::Nothing => PaletteAction::Run(**entry),
                    Some(_) => PaletteAction::Close,
                    None => PaletteAction::Continue,
                };
            }
            KeyCode::Down | KeyCode::Tab => {
                self.cursor = (self.cursor + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char('n') if ctrl => {
                self.cursor = (self.cursor + 1).min(count.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::BackTab => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('p') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::PageDown => self.cursor = (self.cursor + 10).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(10),
            _ if !ctrl && edit_text(&mut self.query, key) => self.cursor = 0,
            _ => {}
        }
        PaletteAction::Continue
    }

    /// A key event for an entry that presses a key.
    pub fn key_event(entry: &Entry) -> Option<KeyEvent> {
        match entry.action {
            Do::Key(code) => Some(KeyEvent::new(code, KeyModifiers::NONE)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_limited_to_the_view_and_filtered_by_words() {
        let mut palette = Palette::new(Where::Experiments);
        assert!(palette.matches().iter().any(|e| e.keys == "z"));
        assert!(!palette.matches().iter().any(|e| e.place == Where::Pipeline));
        palette.query = "copy cell".into();
        let found = palette.matches();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].keys, "y");
    }

    #[test]
    fn typing_filters_and_enter_runs_the_chosen_entry() {
        let mut palette = Palette::new(Where::Experiments);
        for c in "only marked".chars() {
            palette.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let PaletteAction::Run(entry) =
            palette.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("expected a run");
        };
        assert_eq!(entry.action, Do::Key(KeyCode::Char('z')));
        assert_eq!(
            palette.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PaletteAction::Close
        );
    }
}
