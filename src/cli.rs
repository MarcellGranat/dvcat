//! Command-line argument handling.

use std::path::PathBuf;

use anyhow::{Result, bail};

pub const USAGE: &str = "usage: dvcat [PROJECT_DIR]";

/// Parse arguments (the first item is the program name, as in `std::env::args()`).
/// Returns the project directory, defaulting to the current directory.
pub fn project_dir_from_args(mut args: impl Iterator<Item = String>) -> Result<PathBuf> {
    let _program = args.next();
    let dir = match args.next() {
        None => PathBuf::from("."),
        Some(arg) if arg.starts_with('-') => bail!("unknown option '{arg}'\n{USAGE}"),
        Some(arg) => PathBuf::from(arg),
    };
    if let Some(extra) = args.next() {
        bail!("unexpected argument '{extra}'\n{USAGE}");
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<PathBuf> {
        project_dir_from_args(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults_to_current_directory() {
        assert_eq!(parse(&["dvcat"]).unwrap(), PathBuf::from("."));
    }

    #[test]
    fn takes_a_project_directory() {
        assert_eq!(
            parse(&["dvcat", "/some/dir"]).unwrap(),
            PathBuf::from("/some/dir")
        );
    }

    #[test]
    fn rejects_options_and_extra_arguments_with_usage() {
        let err = parse(&["dvcat", "--help"]).unwrap_err().to_string();
        assert!(err.contains("--help") && err.contains("usage:"), "{err}");
        let err = parse(&["dvcat", "a", "b"]).unwrap_err().to_string();
        assert!(err.contains("'b'") && err.contains("usage:"), "{err}");
    }
}
