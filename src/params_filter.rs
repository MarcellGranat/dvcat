//! Choosing which param columns the experiments table shows. Params are grouped the way their
//! labels are built (`file:group.key`), so a whole file or group can be hidden at once.

use std::collections::BTreeSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// One line of the picker: a file, a group inside it, or a single param.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    /// 0 for a file, 1 for a group (or a param directly in a file), 2 for a param in a group.
    pub depth: usize,
    /// The column labels this entry stands for (just one for a single param).
    pub members: Vec<String>,
}

/// How many of an entry's columns are on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    All,
    Some,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    Continue,
    Close,
    /// The hidden set changed.
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamPicker {
    pub entries: Vec<Entry>,
    pub cursor: usize,
}

/// Split `file:group.rest` into the file, the group (when the key has a dot) and the remaining key.
fn split_label(label: &str) -> (&str, Option<&str>, &str) {
    let (file, key) = label.split_once(':').unwrap_or(("", label));
    match key.split_once('.') {
        Some((group, rest)) => (file, Some(group), rest),
        None => (file, None, key),
    }
}

impl ParamPicker {
    /// The entries for `labels` (sorted, as `Table::param_columns` is).
    pub fn new(labels: &[String]) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        let mut files: Vec<&str> = labels.iter().map(|l| split_label(l).0).collect();
        files.dedup();
        for file in files {
            let in_file: Vec<&String> =
                labels.iter().filter(|l| split_label(l).0 == file).collect();
            entries.push(Entry {
                text: if file.is_empty() {
                    "(params)".to_owned()
                } else {
                    file.to_owned()
                },
                depth: 0,
                members: in_file.iter().map(|l| (*l).clone()).collect(),
            });
            let mut groups: Vec<Option<&str>> = in_file.iter().map(|l| split_label(l).1).collect();
            groups.dedup();
            for group in groups {
                let members: Vec<&&String> = in_file
                    .iter()
                    .filter(|l| split_label(l).1 == group)
                    .collect();
                match group {
                    // A group of one is shown as the param itself, with its whole key.
                    Some(name) if members.len() > 1 => {
                        entries.push(Entry {
                            text: format!("{name}.*"),
                            depth: 1,
                            members: members.iter().map(|l| (**l).clone()).collect(),
                        });
                        for label in members {
                            entries.push(Entry {
                                text: split_label(label).2.to_owned(),
                                depth: 2,
                                members: vec![(**label).clone()],
                            });
                        }
                    }
                    _ => {
                        for label in members {
                            let (_, group, rest) = split_label(label);
                            let text =
                                group.map_or_else(|| rest.to_owned(), |g| format!("{g}.{rest}"));
                            entries.push(Entry {
                                text,
                                depth: 1,
                                members: vec![(**label).clone()],
                            });
                        }
                    }
                }
            }
        }
        Self { entries, cursor: 0 }
    }

    pub fn shown(&self, index: usize, hidden: &BTreeSet<String>) -> Shown {
        let Some(entry) = self.entries.get(index) else {
            return Shown::None;
        };
        let visible = entry
            .members
            .iter()
            .filter(|m| !hidden.contains(*m))
            .count();
        match visible {
            0 => Shown::None,
            n if n == entry.members.len() => Shown::All,
            _ => Shown::Some,
        }
    }

    /// Hide the entry under the cursor, or show it when nothing of it is on screen.
    fn toggle(&self, hidden: &mut BTreeSet<String>) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return;
        };
        if self.shown(self.cursor, hidden) == Shown::None {
            for member in &entry.members {
                hidden.remove(member);
            }
        } else {
            hidden.extend(entry.members.iter().cloned());
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, hidden: &mut BTreeSet<String>) -> PickerAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(self.entries.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle(hidden);
                return PickerAction::Changed;
            }
            KeyCode::Char('a') => {
                hidden.clear();
                return PickerAction::Changed;
            }
            KeyCode::Esc | KeyCode::Char('v' | 'q') => return PickerAction::Close,
            _ => {}
        }
        PickerAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Vec<String> {
        [
            "params.json:download.delay",
            "params.json:download.retries",
            "params.json:nimble.model",
            "params.yaml:lr",
            "params.yaml:train.epochs",
            "params.yaml:train.lr",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn texts(picker: &ParamPicker) -> Vec<String> {
        picker
            .entries
            .iter()
            .map(|e| format!("{}{}", "  ".repeat(e.depth), e.text))
            .collect()
    }

    #[test]
    fn files_then_groups_then_params_and_a_lone_param_keeps_its_whole_key() {
        let picker = ParamPicker::new(&labels());
        assert_eq!(
            texts(&picker),
            [
                "params.json",
                "  download.*",
                "    delay",
                "    retries",
                "  nimble.model",
                "params.yaml",
                "  lr",
                "  train.*",
                "    epochs",
                "    lr",
            ]
        );
    }

    #[test]
    fn toggling_a_group_hides_all_of_it_and_again_shows_all_of_it() {
        let mut picker = ParamPicker::new(&labels());
        let mut hidden = BTreeSet::new();
        picker.cursor = 1; // download.*
        picker.toggle(&mut hidden);
        assert_eq!(hidden.len(), 2);
        assert_eq!(picker.shown(1, &hidden), Shown::None);
        assert_eq!(
            picker.shown(0, &hidden),
            Shown::Some,
            "the file is partly hidden"
        );
        picker.toggle(&mut hidden);
        assert!(hidden.is_empty());
    }

    #[test]
    fn a_partly_hidden_entry_is_hidden_completely_first() {
        let mut picker = ParamPicker::new(&labels());
        let mut hidden = BTreeSet::from(["params.json:download.delay".to_owned()]);
        picker.cursor = 1;
        assert_eq!(picker.shown(1, &hidden), Shown::Some);
        picker.toggle(&mut hidden);
        assert_eq!(picker.shown(1, &hidden), Shown::None);
    }

    #[test]
    fn a_whole_file_can_be_hidden_and_a_empty_picker_is_harmless() {
        let picker = ParamPicker::new(&labels());
        let mut hidden = BTreeSet::new();
        picker.toggle(&mut hidden); // cursor 0: params.json
        assert_eq!(hidden.len(), 3);
        let empty = ParamPicker::new(&[]);
        empty.toggle(&mut hidden);
        assert_eq!(empty.shown(0, &hidden), Shown::None);
    }

    #[test]
    fn keys_move_toggle_show_all_and_close() {
        let mut picker = ParamPicker::new(&labels());
        let mut hidden = BTreeSet::new();
        let key = |code| KeyEvent::from(code);
        assert_eq!(
            picker.on_key(key(KeyCode::Char('j')), &mut hidden),
            PickerAction::Continue
        );
        assert_eq!(picker.cursor, 1);
        assert_eq!(
            picker.on_key(key(KeyCode::Char(' ')), &mut hidden),
            PickerAction::Changed
        );
        assert!(!hidden.is_empty());
        assert_eq!(
            picker.on_key(key(KeyCode::Char('a')), &mut hidden),
            PickerAction::Changed
        );
        assert!(hidden.is_empty());
        assert_eq!(
            picker.on_key(key(KeyCode::Esc), &mut hidden),
            PickerAction::Close
        );
    }
}
