//! The "new experiment" form: a name plus one editable value per param column.
//!
//! Pure state and key handling, no drawing and no I/O, so it is easy to test.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    experiments::{Column, Row, Table, cell_text},
    runner,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// `params.yaml:train.lr`, exactly what `dvc exp run -S` expects before the `=`.
    pub label: String,
    /// The value in the row the form was opened on.
    pub original: String,
    pub value: String,
}

impl FormField {
    pub fn is_changed(&self) -> bool {
        self.value != self.original
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentForm {
    /// Name of the row the values were copied from (shown in the title).
    pub base: String,
    pub name: String,
    pub fields: Vec<FormField>,
    /// 0 is the name; 1.. are `fields[active - 1]`.
    pub active: usize,
    /// Why the last Enter was refused.
    pub error: Option<String>,
}

/// What the app should do after a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAction {
    Continue,
    Cancel,
    /// Run `dvc <args>`; `label` names the run in the UI.
    Submit {
        label: String,
        args: Vec<String>,
    },
}

impl ExperimentForm {
    /// Start from the values of `row` (or empty ones when there is no row).
    pub fn from_table(table: &Table, row: Option<&Row>) -> Self {
        let fields = table
            .param_columns
            .iter()
            .map(|label| {
                let original = row
                    .map(|row| cell_text(row, &Column::Param(label.clone())))
                    .unwrap_or_default();
                FormField {
                    label: label.clone(),
                    value: original.clone(),
                    original,
                }
            })
            .collect();
        Self {
            base: row.map_or_else(String::new, |row| row.label.clone()),
            name: String::new(),
            fields,
            active: 0,
            error: None,
        }
    }

    pub fn changed(&self) -> impl Iterator<Item = &FormField> {
        self.fields.iter().filter(|field| field.is_changed())
    }

    fn active_text(&mut self) -> &mut String {
        match self.active {
            0 => &mut self.name,
            n => &mut self.fields[n - 1].value,
        }
    }

    fn move_active(&mut self, forward: bool) {
        let count = self.fields.len() + 1;
        self.active = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> FormAction {
        self.error = None;
        match key.code {
            KeyCode::Esc => return FormAction::Cancel,
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.move_active(true),
            KeyCode::BackTab | KeyCode::Up => self.move_active(false),
            _ => {
                edit_text(self.active_text(), key);
            }
        }
        FormAction::Continue
    }

    fn submit(&mut self) -> FormAction {
        match self.build() {
            Ok((label, args)) => FormAction::Submit { label, args },
            Err(message) => {
                self.error = Some(message);
                FormAction::Continue
            }
        }
    }

    /// The command that Enter would run, for display. `None` while the form is invalid.
    pub fn command_preview(&self) -> Option<String> {
        let (_, args) = self.build().ok()?;
        Some(format!("dvc {}", args.join(" ")))
    }

    fn build(&self) -> Result<(String, Vec<String>), String> {
        let name = self.name.trim();
        if !name.is_empty() && !valid_name(name) {
            return Err("name may only contain letters, digits, '-', '_' and '.'".to_owned());
        }
        let mut overrides = Vec::new();
        for field in self.changed() {
            let value = field.value.trim();
            if value.is_empty() {
                return Err(format!(
                    "{} is empty (type `null` to set a null value)",
                    field.label
                ));
            }
            overrides.push((field.label.clone(), value.to_owned()));
        }
        let name = (!name.is_empty()).then_some(name);
        let label = format!("exp {}", name.unwrap_or("(auto name)"));
        Ok((label, runner::exp_run_args(name, &overrides)))
    }
}

/// Apply a key to a line of text: characters append, Backspace deletes, Ctrl+U clears.
/// Returns whether the key edited the text (other Ctrl/Alt combinations are ignored, not typed).
pub fn edit_text(text: &mut String, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Backspace => {
            text.pop();
            true
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            text.clear();
            true
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            text.push(c);
            true
        }
        _ => false,
    }
}

fn valid_name(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::experiments;

    fn two() -> Table {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        experiments::parse(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn form() -> ExperimentForm {
        let table = two();
        // The baseline row has lr 0.01, epochs 5.
        let row = table.rows.iter().find(|r| r.label == "master").unwrap();
        ExperimentForm::from_table(&table, Some(row))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(form: &mut ExperimentForm, text: &str) {
        for c in text.chars() {
            form.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn fields_start_from_the_values_of_the_chosen_row() {
        let form = form();
        assert_eq!(form.base, "master");
        let pairs: Vec<(&str, &str)> = form
            .fields
            .iter()
            .map(|f| (f.label.as_str(), f.value.as_str()))
            .collect();
        assert!(
            pairs.contains(&("params.yaml:train.lr", "0.01")),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("params.yaml:train.epochs", "5")),
            "{pairs:?}"
        );
        assert_eq!(form.changed().count(), 0);
    }

    #[test]
    fn tab_cycles_through_name_and_fields_in_both_directions() {
        let mut form = form();
        let count = form.fields.len() + 1;
        for _ in 0..count {
            form.on_key(key(KeyCode::Tab));
        }
        assert_eq!(form.active, 0, "wraps around");
        form.on_key(key(KeyCode::BackTab));
        assert_eq!(form.active, count - 1);
    }

    #[test]
    fn typing_edits_only_the_active_field() {
        let mut form = form();
        type_text(&mut form, "my-exp");
        assert_eq!(form.name, "my-exp");
        form.on_key(key(KeyCode::Tab));
        let before = form.fields[0].value.clone();
        form.on_key(key(KeyCode::Backspace));
        type_text(&mut form, "9");
        assert_eq!(
            form.fields[0].value,
            format!("{}9", &before[..before.len() - 1])
        );
        assert!(form.fields[0].is_changed());
        assert_eq!(form.name, "my-exp");
    }

    #[test]
    fn ctrl_u_clears_and_other_ctrl_keys_type_nothing() {
        let mut form = form();
        type_text(&mut form, "abc");
        form.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(form.name, "abc");
        form.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(form.name, "");
    }

    #[test]
    fn enter_builds_the_command_from_the_changed_fields_only() {
        let mut form = form();
        type_text(&mut form, "lr-020");
        let lr = form
            .fields
            .iter()
            .position(|f| f.label == "params.yaml:train.lr")
            .unwrap();
        form.active = lr + 1;
        form.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut form, "0.2");
        let action = form.on_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            FormAction::Submit {
                label: "exp lr-020".into(),
                args: [
                    "exp",
                    "run",
                    "--temp",
                    "-n",
                    "lr-020",
                    "-S",
                    "params.yaml:train.lr=0.2"
                ]
                .map(String::from)
                .into(),
            }
        );
    }

    #[test]
    fn an_unchanged_form_without_a_name_runs_with_an_automatic_name() {
        let action = form().on_key(key(KeyCode::Enter));
        assert_eq!(
            action,
            FormAction::Submit {
                label: "exp (auto name)".into(),
                args: ["exp", "run", "--temp"].map(String::from).into(),
            }
        );
    }

    #[test]
    fn bad_names_and_empty_values_are_refused_with_a_message() {
        let mut form = form();
        type_text(&mut form, "bad name");
        assert_eq!(form.on_key(key(KeyCode::Enter)), FormAction::Continue);
        assert!(form.error.as_deref().unwrap().contains("name may only"));

        form.name = "ok".into();
        form.fields[0].value.clear();
        assert_eq!(form.on_key(key(KeyCode::Enter)), FormAction::Continue);
        assert!(
            form.error.as_deref().unwrap().contains("is empty"),
            "{:?}",
            form.error
        );
        // The next key press clears the message.
        form.on_key(key(KeyCode::Tab));
        assert_eq!(form.error, None);
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(form().on_key(key(KeyCode::Esc)), FormAction::Cancel);
    }

    #[test]
    fn a_form_without_a_row_has_empty_values() {
        let form = ExperimentForm::from_table(&two(), None);
        assert!(form.fields.iter().all(|f| f.value.is_empty()));
        assert_eq!(form.base, "");
    }
}
