//! DVC remotes: reading `dvc remote list` and choosing one for `dvc pull` / `dvc push`.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// Which way data moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Pull,
    Push,
}

impl Transfer {
    /// The `dvc` subcommand.
    pub fn command(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    /// The remote DVC uses when no `-r` is given.
    pub is_default: bool,
}

/// Parse `dvc remote list` (`name<whitespace>url` per line); `default` names the default remote.
pub fn parse(list: &str, default: &str) -> Vec<Remote> {
    let default = default.trim();
    list.lines()
        .filter_map(|line| {
            let (name, url) = line.trim().split_once(char::is_whitespace)?;
            Some(Remote {
                name: name.to_owned(),
                url: url.trim().to_owned(),
                is_default: name == default,
            })
        })
        .collect()
}

/// Run `dvc remote list` and `dvc remote default` in `root`. Blocking: use a background thread.
pub fn fetch(dvc: &Path, root: &Path) -> Result<Vec<Remote>, String> {
    let run = |args: &[&str]| -> Result<String, String> {
        let output = Command::new(dvc)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot start {}: {error}", dvc.display()))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("dvc {} failed: {}", args.join(" "), stderr.trim()))
        }
    };
    let list = run(&["remote", "list"])?;
    // Having no default is normal, so a failure there is not an error.
    let default = run(&["remote", "default"]).unwrap_or_default();
    Ok(parse(&list, &default))
}

/// The list of remotes in the picker popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePicker {
    pub transfer: Transfer,
    pub remotes: Vec<Remote>,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteChoice {
    Continue,
    Cancel,
    Chosen(Remote),
}

impl RemotePicker {
    /// The cursor starts on the default remote.
    pub fn new(transfer: Transfer, remotes: Vec<Remote>) -> Self {
        let cursor = remotes.iter().position(|r| r.is_default).unwrap_or(0);
        Self {
            transfer,
            remotes,
            cursor,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> RemoteChoice {
        let last = self.remotes.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(remote) = self.remotes.get(self.cursor) {
                    return RemoteChoice::Chosen(remote.clone());
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return RemoteChoice::Cancel,
            _ => {}
        }
        RemoteChoice::Continue
    }
}

/// `dvc pull|push -r <remote>`.
pub fn transfer_args(transfer: Transfer, remote: &str) -> Vec<String> {
    vec![
        transfer.command().to_owned(),
        "-r".to_owned(),
        remote.to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    fn remotes() -> Vec<Remote> {
        parse(
            "origin\ts3://bucket/data\nbackup\t/mnt/backup\n",
            "backup\n",
        )
    }

    #[test]
    fn parses_names_urls_and_the_default() {
        assert_eq!(
            remotes(),
            [
                Remote {
                    name: "origin".into(),
                    url: "s3://bucket/data".into(),
                    is_default: false,
                },
                Remote {
                    name: "backup".into(),
                    url: "/mnt/backup".into(),
                    is_default: true,
                },
            ]
        );
        assert!(parse("", "").is_empty());
    }

    #[test]
    fn the_cursor_starts_on_the_default_and_enter_chooses() {
        let mut picker = RemotePicker::new(Transfer::Push, remotes());
        assert_eq!(picker.cursor, 1);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            picker.on_key(key(KeyCode::Char('k'))),
            RemoteChoice::Continue
        );
        match picker.on_key(key(KeyCode::Enter)) {
            RemoteChoice::Chosen(remote) => assert_eq!(remote.name, "origin"),
            other => panic!("{other:?}"),
        }
        assert_eq!(picker.on_key(key(KeyCode::Esc)), RemoteChoice::Cancel);
    }

    #[test]
    fn builds_the_arguments() {
        assert_eq!(
            transfer_args(Transfer::Pull, "origin"),
            ["pull", "-r", "origin"]
        );
    }
}
