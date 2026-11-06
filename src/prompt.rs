//! A one-line text prompt with validation, shown as a popup (used to name a Git branch).

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::form::edit_text;

#[derive(Debug, Clone)]
pub struct Prompt {
    pub title: String,
    pub label: String,
    /// The experiment the answer is about.
    pub target: String,
    pub value: String,
    /// Why the last Enter was refused.
    pub error: Option<String>,
    validate: fn(&str) -> Result<(), String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    Continue,
    Cancel,
    Submit(String),
}

impl Prompt {
    /// Ask for the name of a Git branch for `experiment`, suggesting `<experiment>-branch`.
    pub fn branch(experiment: &str) -> Self {
        Self {
            title: format!("Branch from {experiment}"),
            label: "branch name".to_owned(),
            target: experiment.to_owned(),
            value: format!("{experiment}-branch"),
            error: None,
            validate: validate_branch_name,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PromptAction {
        self.error = None;
        match key.code {
            KeyCode::Esc => PromptAction::Cancel,
            KeyCode::Enter => {
                let name = self.value.trim().to_owned();
                match (self.validate)(&name) {
                    Ok(()) => PromptAction::Submit(name),
                    Err(message) => {
                        self.error = Some(message);
                        PromptAction::Continue
                    }
                }
            }
            _ => {
                edit_text(&mut self.value, key);
                PromptAction::Continue
            }
        }
    }
}

/// A conservative subset of `git check-ref-format`, enough to explain the problem before DVC runs.
/// Git has the last word: anything it still rejects shows up in the run's log.
pub fn validate_branch_name(name: &str) -> Result<(), String> {
    let bad = |message: &str| Err(format!("{message}: '{name}'"));
    if name.is_empty() {
        return Err("the branch name is empty".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(*c))
    {
        return Err(format!(
            "'{}' is not allowed in a branch name",
            c.escape_debug()
        ));
    }
    if name.contains("..") || name.contains("@{") {
        return bad("'..' and '@{' are not allowed");
    }
    if name.starts_with('-') {
        return bad("a branch name cannot start with '-'");
    }
    if name == "@" || name.ends_with('.') || name.ends_with('/') {
        return bad("a branch name cannot end with '.' or '/' or be '@'");
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return bad("no part of the name may be empty, start with '.' or end with '.lock'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn starts_with_the_suggested_name() {
        let prompt = Prompt::branch("lr-005");
        assert_eq!(prompt.value, "lr-005-branch");
        assert_eq!(prompt.title, "Branch from lr-005");
    }

    #[test]
    fn enter_submits_the_default_and_an_edited_name() {
        let mut prompt = Prompt::branch("lr-005");
        assert_eq!(
            prompt.on_key(key(KeyCode::Enter)),
            PromptAction::Submit("lr-005-branch".into())
        );
        for _ in 0.."-branch".len() {
            prompt.on_key(key(KeyCode::Backspace));
        }
        for c in "-best".chars() {
            prompt.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            prompt.on_key(key(KeyCode::Enter)),
            PromptAction::Submit("lr-005-best".into())
        );
    }

    #[test]
    fn esc_cancels_and_ctrl_u_clears() {
        let mut prompt = Prompt::branch("a");
        assert_eq!(prompt.on_key(key(KeyCode::Esc)), PromptAction::Cancel);
        prompt.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(prompt.value, "");
    }

    #[test]
    fn an_invalid_name_keeps_the_prompt_open_with_a_message() {
        let mut prompt = Prompt::branch("a");
        prompt.value = "bad name".into();
        assert_eq!(prompt.on_key(key(KeyCode::Enter)), PromptAction::Continue);
        assert!(prompt.error.as_deref().unwrap().contains("not allowed"));
        prompt.on_key(key(KeyCode::Char('x')));
        assert_eq!(prompt.error, None, "the next key clears the message");
    }

    #[test]
    fn every_invalid_form_is_refused() {
        for name in [
            "",
            "a b",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            "a..b",
            "a@{b",
            "-a",
            ".a",
            "a/",
            "a.",
            "@",
            "a.lock",
            "x/.hidden",
            "x//y",
            "x/y.lock",
            "tab\tname",
        ] {
            assert!(
                validate_branch_name(name).is_err(),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["lr-005-branch", "feature/lr_5", "exp.v2", "a", "ünï"] {
            assert!(
                validate_branch_name(name).is_ok(),
                "{name:?} should be accepted"
            );
        }
    }
}
