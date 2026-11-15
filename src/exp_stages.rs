//! Which stages ran in experiments. `dvc exp run --temp` works in a copy of the workspace, so its
//! results go into the experiment's own `dvc.lock` and the workspace lock never learns of them.

use std::{collections::BTreeMap, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Stage names listed under `stages:` of a `dvc.lock`.
pub fn stage_names(lock: &str) -> Vec<String> {
    let mut inside = false;
    let mut names = Vec::new();
    for line in lock.lines() {
        if !line.starts_with(' ') && !line.is_empty() {
            inside = line.trim_end() == "stages:";
        } else if inside
            && let Some(name) = line
                .strip_prefix("  ")
                .filter(|rest| !rest.starts_with(' '))
                .and_then(|rest| rest.trim_end().strip_suffix(':'))
        {
            names.push(name.trim_matches(['\'', '"']).to_owned());
        }
    }
    names
}

/// Stage name -> in how many experiments it ran. Empty when git or the experiments are missing.
pub fn count(root: &Path) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let Some(refs) = git(root, &["for-each-ref", "--format=%(refname)", "refs/exps"]) else {
        return counts;
    };
    for reference in refs.lines() {
        let Some(lock) = git(root, &["show", &format!("{reference}:dvc.lock")]) else {
            continue;
        };
        for name in stage_names(&lock) {
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_are_read_from_the_stages_section_only() {
        let lock = "schema: '2.0'\nstages:\n  panel:\n    cmd: x\n    deps:\n    - path: a\n  split@0:\n    cmd: y\n  'odd name':\n    cmd: z\n";
        assert_eq!(stage_names(lock), ["panel", "split@0", "odd name"]);
        assert!(stage_names("schema: '2.0'\n").is_empty());
    }
}
