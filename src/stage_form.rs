//! The "new stage" form: name, command, the lists of deps, params, outs and metrics, and the
//! on/off options. Pure state and key handling plus the text written to `dvc.yaml`; no drawing, no I/O.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    dvc_yaml::{self, scalar},
    form::edit_text,
    project::DEFAULT_PARAMS_FILE,
};

/// What `fzf` is asked to choose from for a list field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Deps,
    Params,
    Outs,
    Metrics,
}

pub const NAME: usize = 0;
pub const CMD: usize = 1;
pub const DEPS: usize = 2;
pub const PARAMS: usize = 3;
pub const OUTS: usize = 4;
pub const METRICS: usize = 5;
/// Number of text fields; the toggles come after them.
pub const TEXTS: usize = 6;

/// Label and hint of each text field.
pub const TEXT_FIELDS: [(&str, &str); TEXTS] = [
    ("name", "stage name: letters, digits, '-', '_' and '@'"),
    ("cmd", "command to run, for example: python src/train.py"),
    (
        "deps",
        "dependencies, comma separated (Ctrl+F: pick files with fzf)",
    ),
    (
        "params",
        "params, comma separated: train.lr or other.yaml:key (Ctrl+F: pick with fzf)",
    ),
    (
        "outs",
        "outputs, comma separated (Ctrl+F: pick existing files with fzf)",
    ),
    (
        "metrics",
        "metrics files, comma separated; never cached (Ctrl+F: pick with fzf)",
    ),
];

/// Label and hint of each toggle, in the order of [`StageForm::flags`].
pub const TOGGLES: [(&str, &str); 4] = [
    (
        "cache",
        "keep the outputs in the DVC cache (off: Git keeps them, like metrics)",
    ),
    (
        "persist",
        "do not delete the outputs before the stage runs again",
    ),
    (
        "always_changed",
        "run the stage every time, even when nothing changed",
    ),
    (
        "frozen",
        "never rerun the stage, even when its dependencies change",
    ),
];
const CACHE: usize = 0;
const PERSIST: usize = 1;
const ALWAYS_CHANGED: usize = 2;
const FROZEN: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageForm {
    pub texts: [String; TEXTS],
    /// cache, persist, always_changed, frozen.
    pub flags: [bool; 4],
    /// 0..TEXTS are the text fields, then the toggles.
    pub active: usize,
    /// Why the last Enter was refused.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageFormAction {
    Continue,
    Cancel,
    /// Ctrl+F on a list field: the app lets the user pick with `fzf`.
    Pick(Pick),
    Submit(StageDraft),
}

impl Default for StageForm {
    fn default() -> Self {
        Self {
            texts: Default::default(),
            // Outputs are cached unless told otherwise.
            flags: [true, false, false, false],
            active: NAME,
            error: None,
        }
    }
}

/// The comma separated entries of a list field.
fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

impl StageForm {
    pub fn field_count() -> usize {
        TEXTS + TOGGLES.len()
    }

    pub fn is_toggle(&self) -> bool {
        self.active >= TEXTS
    }

    pub fn on_key(&mut self, key: KeyEvent) -> StageFormAction {
        self.error = None;
        let count = Self::field_count();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return StageFormAction::Cancel,
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.active = (self.active + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.active = (self.active + count - 1) % count,
            KeyCode::Char('f') if ctrl => {
                return match self.active {
                    DEPS => StageFormAction::Pick(Pick::Deps),
                    PARAMS => StageFormAction::Pick(Pick::Params),
                    OUTS => StageFormAction::Pick(Pick::Outs),
                    METRICS => StageFormAction::Pick(Pick::Metrics),
                    _ => StageFormAction::Continue,
                };
            }
            // A toggle flips with Space or the arrows; Enter still submits.
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right if self.is_toggle() => {
                let flag = &mut self.flags[self.active - TEXTS];
                *flag = !*flag;
            }
            _ if self.is_toggle() => {}
            _ => {
                edit_text(&mut self.texts[self.active], key);
            }
        }
        StageFormAction::Continue
    }

    /// Add what `fzf` chose to the list field the pick was for.
    pub fn add_picked(&mut self, pick: Pick, items: &[String]) {
        let field = match pick {
            Pick::Deps => DEPS,
            Pick::Params => PARAMS,
            Pick::Outs => OUTS,
            Pick::Metrics => METRICS,
        };
        let mut all = split_list(&self.texts[field]);
        for item in items {
            if !all.contains(item) {
                all.push(item.clone());
            }
        }
        self.texts[field] = all.join(", ");
        self.active = field;
    }

    fn submit(&mut self) -> StageFormAction {
        match self.build() {
            Ok(draft) => StageFormAction::Submit(draft),
            Err(message) => {
                self.error = Some(message);
                StageFormAction::Continue
            }
        }
    }

    fn build(&self) -> Result<StageDraft, String> {
        let name = self.texts[NAME].trim();
        if name.is_empty() {
            return Err("the stage needs a name".to_owned());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@'))
        {
            return Err("name may only contain letters, digits, '-', '_' and '@'".to_owned());
        }
        let cmd = self.texts[CMD].trim();
        if cmd.is_empty() {
            return Err("the stage needs a command".to_owned());
        }
        Ok(StageDraft {
            name: name.to_owned(),
            cmd: cmd.to_owned(),
            deps: split_list(&self.texts[DEPS]),
            params: split_list(&self.texts[PARAMS]),
            outs: split_list(&self.texts[OUTS]),
            metrics: split_list(&self.texts[METRICS]),
            cache: self.flags[CACHE],
            persist: self.flags[PERSIST],
            always_changed: self.flags[ALWAYS_CHANGED],
            frozen: self.flags[FROZEN],
        })
    }
}

/// A stage as the form describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StageDraft {
    pub name: String,
    pub cmd: String,
    pub deps: Vec<String>,
    /// `key` (in `params.yaml`) or `file:key`.
    pub params: Vec<String>,
    pub outs: Vec<String>,
    pub metrics: Vec<String>,
    pub cache: bool,
    pub persist: bool,
    pub always_changed: bool,
    pub frozen: bool,
}

impl StageDraft {
    /// The stage for `dvc.yaml`, its name indented by `indent` spaces.
    pub fn entry(&self, indent: usize) -> String {
        let pad = |extra: usize| " ".repeat(indent + extra);
        let mut text = format!(
            "{}{}:\n{}cmd: {}\n",
            pad(0),
            self.name,
            pad(2),
            scalar(&self.cmd)
        );
        if !self.deps.is_empty() {
            text.push_str(&format!("{}deps:\n", pad(2)));
            for dep in &self.deps {
                text.push_str(&format!("{}- {}\n", pad(4), scalar(dep)));
            }
        }
        if !self.params.is_empty() {
            text.push_str(&format!("{}params:\n", pad(2)));
            // Keys of the default file are plain list items; other files list their keys.
            let mut other: Vec<(&str, Vec<&str>)> = Vec::new();
            for param in &self.params {
                match param.split_once(':') {
                    Some((file, key)) if file != DEFAULT_PARAMS_FILE => {
                        match other.iter_mut().find(|(f, _)| *f == file) {
                            Some((_, keys)) => keys.push(key),
                            None => other.push((file, vec![key])),
                        }
                    }
                    Some((_, key)) => text.push_str(&format!("{}- {}\n", pad(4), scalar(key))),
                    None => text.push_str(&format!("{}- {}\n", pad(4), scalar(param))),
                }
            }
            for (file, keys) in other {
                text.push_str(&format!("{}- {}:\n", pad(4), scalar(file)));
                for key in keys {
                    text.push_str(&format!("{}- {}\n", pad(8), scalar(key)));
                }
            }
        }
        let options = |text: &mut String, path: &str, options: &[&str]| {
            if options.is_empty() {
                text.push_str(&format!("{}- {}\n", pad(4), scalar(path)));
            } else {
                text.push_str(&format!("{}- {}:\n", pad(4), scalar(path)));
                for option in options {
                    text.push_str(&format!("{}{option}\n", pad(8)));
                }
            }
        };
        if !self.outs.is_empty() {
            text.push_str(&format!("{}outs:\n", pad(2)));
            let mut set: Vec<&str> = Vec::new();
            if !self.cache {
                set.push("cache: false");
            }
            if self.persist {
                set.push("persist: true");
            }
            for out in &self.outs {
                options(&mut text, out, &set);
            }
        }
        if !self.metrics.is_empty() {
            text.push_str(&format!("{}metrics:\n", pad(2)));
            for metric in &self.metrics {
                options(&mut text, metric, &["cache: false"]);
            }
        }
        if self.always_changed {
            text.push_str(&format!("{}always_changed: true\n", pad(2)));
        }
        if self.frozen {
            text.push_str(&format!("{}frozen: true\n", pad(2)));
        }
        text
    }
}

/// `dvc.yaml` with the stage added to its `stages:` section (created when missing).
pub fn add_to_dvc_yaml(text: &str, draft: &StageDraft) -> Result<String, String> {
    dvc_yaml::insert_into_section(text, "stages", |indent| draft.entry(indent), |_| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::parse_dvc_yaml;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(form: &mut StageForm, text: &str) {
        for c in text.chars() {
            form.on_key(key(KeyCode::Char(c)));
        }
    }

    fn filled() -> StageForm {
        let mut form = StageForm::default();
        type_text(&mut form, "train");
        form.active = CMD;
        type_text(&mut form, "python train.py --fast");
        form
    }

    #[test]
    fn typing_goes_to_the_active_field_and_spaces_stay_in_text() {
        let form = filled();
        assert_eq!(form.texts[NAME], "train");
        assert_eq!(form.texts[CMD], "python train.py --fast");
    }

    #[test]
    fn toggles_flip_with_space_and_arrows_and_ignore_typing() {
        let mut form = StageForm {
            active: TEXTS + PERSIST,
            ..StageForm::default()
        };
        form.on_key(key(KeyCode::Char(' ')));
        assert!(form.flags[PERSIST]);
        form.on_key(key(KeyCode::Left));
        assert!(!form.flags[PERSIST]);
        form.on_key(key(KeyCode::Char('z')));
        assert!(form.texts.iter().all(String::is_empty));
        form.active = TEXTS + CACHE;
        form.on_key(key(KeyCode::Right));
        assert!(!form.flags[CACHE]);
    }

    #[test]
    fn name_and_command_are_required_and_the_name_is_checked() {
        let mut form = StageForm::default();
        form.on_key(key(KeyCode::Enter));
        assert!(form.error.as_deref().unwrap().contains("name"));
        type_text(&mut form, "bad name");
        form.on_key(key(KeyCode::Enter));
        assert!(form.error.as_deref().unwrap().contains("letters"));
        form.texts[NAME] = "ok".to_owned();
        form.on_key(key(KeyCode::Enter));
        assert!(form.error.as_deref().unwrap().contains("command"));
    }

    #[test]
    fn ctrl_f_asks_for_a_pick_on_list_fields_only_and_picks_are_added_once() {
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let mut form = filled();
        assert_eq!(form.on_key(ctrl_f), StageFormAction::Continue);
        form.active = DEPS;
        assert_eq!(form.on_key(ctrl_f), StageFormAction::Pick(Pick::Deps));
        form.add_picked(Pick::Deps, &["a.csv".into(), "b.csv".into()]);
        form.add_picked(Pick::Deps, &["b.csv".into(), "c.csv".into()]);
        assert_eq!(form.texts[DEPS], "a.csv, b.csv, c.csv");
        form.active = PARAMS;
        assert_eq!(form.on_key(ctrl_f), StageFormAction::Pick(Pick::Params));
    }

    #[test]
    fn a_full_draft_is_written_the_way_dvc_reads_it_back() {
        let mut form = filled();
        form.texts[DEPS] = "src/train.py, data/features.csv".to_owned();
        form.texts[PARAMS] = "train.lr, extra.json:alpha, extra.json:beta".to_owned();
        form.texts[OUTS] = "model.txt".to_owned();
        form.texts[METRICS] = "metrics.json".to_owned();
        form.flags = [false, true, true, false];
        let StageFormAction::Submit(draft) = form.on_key(key(KeyCode::Enter)) else {
            panic!("{:?}", form.error)
        };
        let yaml = add_to_dvc_yaml("stages: {}\n", &draft);
        assert!(yaml.is_err(), "an inline section is refused");
        let yaml = add_to_dvc_yaml("stages:\n  prep:\n    cmd: echo\n", &draft).unwrap();
        assert!(
            yaml.contains("      - model.txt:\n          cache: false\n          persist: true\n"),
            "{yaml}"
        );
        assert!(yaml.contains("    always_changed: true\n"), "{yaml}");
        let stages = parse_dvc_yaml(&yaml).unwrap();
        let train = stages.iter().find(|s| s.name == "train").unwrap();
        assert_eq!(train.deps, ["src/train.py", "data/features.csv"]);
        assert_eq!(train.outs, ["model.txt"]);
        assert_eq!(train.metrics, ["metrics.json"]);
        let files: Vec<_> = train
            .params
            .iter()
            .map(|p| (p.file.as_str(), p.keys.clone()))
            .collect();
        assert_eq!(
            files,
            [
                ("params.yaml", vec!["train.lr".to_owned()]),
                ("extra.json", vec!["alpha".to_owned(), "beta".to_owned()])
            ]
        );
    }

    #[test]
    fn a_minimal_draft_has_only_a_command_and_the_section_is_created_when_missing() {
        let StageFormAction::Submit(draft) = filled().on_key(key(KeyCode::Enter)) else {
            panic!()
        };
        let yaml = add_to_dvc_yaml("plots:\n  - a.csv\n", &draft).unwrap();
        assert_eq!(
            yaml,
            "plots:\n  - a.csv\nstages:\n  train:\n    cmd: python train.py --fast\n"
        );
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(
            StageForm::default().on_key(key(KeyCode::Esc)),
            StageFormAction::Cancel
        );
    }
}
