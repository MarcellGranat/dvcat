//! Experiment messages: the text given with `dvc exp run -m "..."`.
//!
//! DVC keeps it as the message of the experiment's Git commit, next to (not inside) the name, and
//! `dvc exp show --json` does not report it, so it is read with `git log`. DVC has no command to
//! change it. Editing therefore writes a copy of the commit with the new message (same tree,
//! parents, author and dates) and moves the experiment's ref (`refs/exps/...`) to it. The name
//! stays; the commit id changes.

use std::{collections::BTreeMap, path::Path, process::Command};

const EXPERIMENT_REFS: &str = "refs/exps";
/// Separates the fields of one record in `git log --format`.
const FIELD: char = '\0';
const RECORD: char = '\u{1}';

fn git(root: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .map_err(|error| format!("cannot start git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Full commit id -> message, for the commits among `revs` that can be read. Anything git cannot
/// resolve (the `workspace` pseudo revision) is left out.
pub fn read_all(root: &Path, revs: &[&str]) -> BTreeMap<String, String> {
    let real: Vec<&str> = revs.iter().copied().filter(|r| *r != "workspace").collect();
    if real.is_empty() {
        return BTreeMap::new();
    }
    let mut args = vec!["log", "--no-walk=unsorted", "--format=%H%x00%B%x01"];
    args.extend(real);
    let Ok(text) = git(root, &args, &[]) else {
        return BTreeMap::new();
    };
    text.split(RECORD)
        .filter_map(|record| {
            let (rev, message) = record.trim_start().split_once(FIELD)?;
            Some((rev.to_owned(), message.trim().to_owned()))
        })
        .collect()
}

/// The first line of a message.
pub fn subject(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// `message` with its first line replaced, keeping the body.
pub fn with_subject(message: &str, subject: &str) -> String {
    match message.split_once('\n') {
        Some((_, body)) => format!("{subject}\n{body}"),
        None => subject.to_owned(),
    }
}

/// Give the experiment commit `rev` a new message. Returns the id of the new commit.
pub fn rewrite(root: &Path, rev: &str, message: &str) -> Result<String, String> {
    let refs = git(
        root,
        &[
            "for-each-ref",
            "--points-at",
            rev,
            "--format=%(refname)",
            EXPERIMENT_REFS,
        ],
        &[],
    )?;
    let refs: Vec<&str> = refs.lines().filter(|line| !line.is_empty()).collect();
    let [reference] = refs.as_slice() else {
        return Err(if refs.is_empty() {
            "this commit is not an experiment (no ref under refs/exps)".to_owned()
        } else {
            "several experiments share this commit; edit it with git".to_owned()
        });
    };

    let tree = git(root, &["rev-parse", &format!("{rev}^{{tree}}")], &[])?;
    let parents = git(root, &["log", "-1", "--format=%P", rev], &[])?;
    let who = git(
        root,
        &[
            "log",
            "-1",
            "--format=%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI",
            rev,
        ],
        &[],
    )?;
    let who: Vec<&str> = who.trim_end().split(FIELD).collect();
    let [an, ae, ad, cn, ce, cd] = who.as_slice() else {
        return Err("could not read the commit's author".to_owned());
    };

    let mut args = vec!["commit-tree", tree.trim(), "-m", message];
    for parent in parents.split_whitespace() {
        args.extend(["-p", parent]);
    }
    let envs = [
        ("GIT_AUTHOR_NAME", *an),
        ("GIT_AUTHOR_EMAIL", *ae),
        ("GIT_AUTHOR_DATE", *ad),
        ("GIT_COMMITTER_NAME", *cn),
        ("GIT_COMMITTER_EMAIL", *ce),
        ("GIT_COMMITTER_DATE", *cd),
    ];
    let new = git(root, &args, &envs)?;
    let new = new.trim();
    let old = git(root, &["rev-parse", rev], &[])?;
    git(root, &["update-ref", reference, new, old.trim()], &[])?;
    Ok(new.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) -> String {
        git(
            root,
            args,
            &[
                ("GIT_AUTHOR_NAME", "a"),
                ("GIT_AUTHOR_EMAIL", "a@x"),
                ("GIT_COMMITTER_NAME", "c"),
                ("GIT_COMMITTER_EMAIL", "c@x"),
            ],
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    /// A repository with a baseline commit and one experiment commit under `refs/exps`.
    fn repo() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q"]);
        std::fs::write(root.join("f"), "1").unwrap();
        run(root, &["add", "f"]);
        run(root, &["commit", "-qm", "baseline"]);
        let base = run(root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("f"), "2").unwrap();
        run(root, &["add", "f"]);
        run(root, &["commit", "-qm", "first line\n\nthe body"]);
        let exp = run(root, &["rev-parse", "HEAD"]);
        run(root, &["update-ref", "refs/exps/ab/cdef/lr-1", &exp]);
        (dir, base, exp)
    }

    #[test]
    fn reads_messages_and_skips_the_workspace() {
        let (dir, base, exp) = repo();
        let all = read_all(dir.path(), &["workspace", &base, &exp]);
        assert_eq!(all.len(), 2);
        assert_eq!(all[&base], "baseline");
        assert_eq!(all[&exp], "first line\n\nthe body");
        assert!(read_all(dir.path(), &["workspace"]).is_empty());
        assert!(read_all(dir.path(), &["not-a-rev"]).is_empty());
    }

    #[test]
    fn subject_and_body() {
        assert_eq!(subject("a\n\nb"), "a");
        assert_eq!(with_subject("a\n\nb", "new"), "new\n\nb");
        assert_eq!(with_subject("a", "new"), "new");
        assert_eq!(subject(""), "");
    }

    #[test]
    fn rewrite_moves_the_ref_and_keeps_tree_parent_and_author() {
        let (dir, base, exp) = repo();
        let root = dir.path();
        let new = rewrite(root, &exp, "better text\n\nthe body").unwrap();
        assert_ne!(new, exp);
        assert_eq!(run(root, &["rev-parse", "refs/exps/ab/cdef/lr-1"]), new);
        assert_eq!(
            run(root, &["log", "-1", "--format=%B", &new]),
            "better text\n\nthe body"
        );
        assert_eq!(run(root, &["rev-parse", &format!("{new}^")]), base);
        assert_eq!(
            run(root, &["rev-parse", &format!("{new}^{{tree}}")]),
            run(root, &["rev-parse", &format!("{exp}^{{tree}}")])
        );
        assert_eq!(
            run(root, &["log", "-1", "--format=%an %ae %aI", &new]),
            run(root, &["log", "-1", "--format=%an %ae %aI", &exp])
        );
    }

    #[test]
    fn rewrite_refuses_commits_that_are_not_experiments() {
        let (dir, base, _) = repo();
        let error = rewrite(dir.path(), &base, "x").unwrap_err();
        assert!(error.contains("not an experiment"), "{error}");
    }
}
