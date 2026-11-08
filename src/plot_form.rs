//! The "new plot" form: the data file plus the optional x, y, template and title.
//!
//! Pure state and key handling, no drawing and no I/O, like [`crate::form`].

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    form::edit_text,
    plots::{Draft, TEMPLATES, Template},
};

/// Label, and what to say about it when the field is empty.
pub const FIELDS: [(&str, &str); 5] = [
    (
        "file",
        "data file, relative to the project (csv, tsv, json, yaml)",
    ),
    ("x", "x field (empty: the row number)"),
    ("y", "y fields, comma separated (empty: the last column)"),
    ("template", TEMPLATES),
    ("title", "optional"),
];
const FILE: usize = 0;
const X: usize = 1;
const Y: usize = 2;
const TEMPLATE: usize = 3;
const TITLE: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlotForm {
    pub values: [String; 5],
    pub active: usize,
    /// Why the last Enter was refused.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlotFormAction {
    Continue,
    Cancel,
    /// Ctrl+F on the file field: the app lets the user pick a file with `fzf`.
    PickFile,
    Submit(Draft),
}

impl Default for PlotForm {
    fn default() -> Self {
        let mut values: [String; 5] = Default::default();
        values[TEMPLATE] = "linear".to_owned();
        Self {
            values,
            active: FILE,
            error: None,
        }
    }
}

impl PlotForm {
    pub fn file(&self) -> &str {
        self.values[FILE].trim()
    }

    pub fn set_file(&mut self, file: &str) {
        self.values[FILE] = file.to_owned();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PlotFormAction {
        self.error = None;
        let count = self.values.len();
        match key.code {
            KeyCode::Esc => return PlotFormAction::Cancel,
            KeyCode::Enter => return self.submit(),
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.active = FILE;
                return PlotFormAction::PickFile;
            }
            KeyCode::Tab | KeyCode::Down => self.active = (self.active + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.active = (self.active + count - 1) % count,
            _ => {
                edit_text(&mut self.values[self.active], key);
            }
        }
        PlotFormAction::Continue
    }

    fn submit(&mut self) -> PlotFormAction {
        match self.build() {
            Ok(draft) => PlotFormAction::Submit(draft),
            Err(message) => {
                self.error = Some(message);
                PlotFormAction::Continue
            }
        }
    }

    fn build(&self) -> Result<Draft, String> {
        let file = self.file();
        if file.is_empty() {
            return Err("the data file is required".to_owned());
        }
        let template = self.values[TEMPLATE].trim();
        if !template.is_empty() && Template::parse(template).is_none() {
            return Err(format!("unknown template '{template}'; one of {TEMPLATES}"));
        }
        Ok(Draft {
            file: file.to_owned(),
            x: self.values[X].trim().to_owned(),
            y: self.values[Y]
                .split(',')
                .map(str::trim)
                .filter(|y| !y.is_empty())
                .map(str::to_owned)
                .collect(),
            template: template.to_owned(),
            title: self.values[TITLE].trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(form: &mut PlotForm, text: &str) {
        for c in text.chars() {
            form.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_goes_to_the_active_field_and_tab_moves_on() {
        let mut form = PlotForm::default();
        type_text(&mut form, "a.csv");
        form.on_key(key(KeyCode::Tab));
        type_text(&mut form, "epoch");
        assert_eq!(
            (form.values[0].as_str(), form.values[1].as_str()),
            ("a.csv", "epoch")
        );
        form.on_key(key(KeyCode::BackTab));
        assert_eq!(form.active, 0);
    }

    #[test]
    fn enter_builds_a_draft_with_split_y_fields() {
        let mut form = PlotForm::default();
        type_text(&mut form, "a.csv");
        form.active = Y;
        type_text(&mut form, "loss, acc");
        let PlotFormAction::Submit(draft) = form.on_key(key(KeyCode::Enter)) else {
            panic!("{:?}", form.error)
        };
        assert_eq!(draft.y, ["loss", "acc"]);
        assert_eq!(draft.template, "linear");
    }

    #[test]
    fn a_missing_file_or_unknown_template_keeps_the_form_open() {
        let mut form = PlotForm::default();
        assert_eq!(form.on_key(key(KeyCode::Enter)), PlotFormAction::Continue);
        assert!(form.error.as_deref().unwrap().contains("required"));
        type_text(&mut form, "a.csv");
        form.active = TEMPLATE;
        type_text(&mut form, "x");
        form.on_key(key(KeyCode::Enter));
        assert!(form.error.as_deref().unwrap().contains("unknown template"));
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(
            PlotForm::default().on_key(key(KeyCode::Esc)),
            PlotFormAction::Cancel
        );
    }

    #[test]
    fn ctrl_f_asks_to_pick_a_file_and_the_pick_fills_the_field() {
        let mut form = PlotForm {
            active: Y,
            ..PlotForm::default()
        };
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(form.on_key(ctrl_f), PlotFormAction::PickFile);
        assert_eq!(form.active, FILE);
        form.set_file("metrics/acc.csv");
        assert_eq!(form.file(), "metrics/acc.csv");
        // Ctrl+F is not typed into the field.
        assert_eq!(form.values[FILE], "metrics/acc.csv");
    }
}
