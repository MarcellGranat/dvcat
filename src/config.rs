//! `.dvcat.yaml` in the project directory: the few things worth setting once.
//!
//! ```yaml
//! theme: latte          # mocha, latte or colour-blind
//! mouse: true           # click rows, headers; the wheel scrolls (steals text selection)
//! better:               # which way a metric improves; a label or a part of its name
//! loss: lower           #   (the names loss, auc, ... are understood without this)
//! coverage_90: higher
//! ```

use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

use crate::{experiments::Better, theme};

const FILE: &str = ".dvcat.yaml";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub theme: Option<theme::Mode>,
    pub mouse: bool,
    /// Metric label (or part of it, lower case) -> direction.
    pub better: BTreeMap<String, Better>,
    /// Why part of the file was not used.
    pub problems: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Raw {
    theme: Option<String>,
    mouse: Option<bool>,
    #[serde(default)]
    better: BTreeMap<String, String>,
}

/// Read the file; a missing file is the default config, a broken one too (with the reason).
pub fn load(root: &Path) -> Config {
    let Ok(text) = fs::read_to_string(root.join(FILE)) else {
        return Config::default();
    };
    parse(&text)
}

pub fn parse(text: &str) -> Config {
    let mut config = Config::default();
    let raw: Raw = match serde_norway::from_str(text) {
        Ok(raw) => raw,
        Err(error) => {
            config.problems.push(format!("{FILE}: {error}"));
            return config;
        }
    };
    config.mouse = raw.mouse.unwrap_or(false);
    if let Some(name) = raw.theme {
        match theme::Mode::parse(&name) {
            Some(mode) => config.theme = Some(mode),
            None => config.problems.push(format!(
                "{FILE}: unknown theme '{name}' (mocha, latte, colour-blind)"
            )),
        }
    }
    for (label, direction) in raw.better {
        match Better::parse(&direction) {
            Some(better) => {
                config.better.insert(label.to_lowercase(), better);
            }
            None => config.problems.push(format!(
                "{FILE}: '{direction}' for {label} is neither lower nor higher"
            )),
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_theme_mouse_and_directions() {
        let config =
            parse("theme: light\nmouse: true\nbetter:\n  Coverage_90: higher\n  gap: lower\n");
        assert_eq!(config.theme, Some(theme::Mode::Latte));
        assert!(config.mouse);
        assert_eq!(config.better["coverage_90"], Better::Higher);
        assert_eq!(config.better["gap"], Better::Lower);
        assert!(config.problems.is_empty());
    }

    #[test]
    fn mistakes_are_reported_and_the_rest_is_kept() {
        let config = parse("theme: neon\nbetter:\n  x: sideways\n  y: lower\n");
        assert_eq!(config.problems.len(), 2, "{:?}", config.problems);
        assert_eq!(config.better.len(), 1);
        let broken = parse("theme: [");
        assert_eq!(broken.problems.len(), 1);
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Config::default());
    }
}
