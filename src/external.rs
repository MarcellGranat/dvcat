//! `dvc` processes that dvcat did not start (another terminal, another tool) and that hold DVC's
//! lock. DVC lists them in `.dvc/tmp/rwlock`; when a run fails because of them, dvcat names them
//! and offers to stop them.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

/// DVC prints this when another process holds a lock the run needs.
const BLOCKED_TEXT: &str = "being blocked by";
/// How long a stopped process gets to exit before it is killed.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// A live `dvc` process, started outside this session, that holds the project's lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRun {
    pub pid: u32,
    /// The command DVC recorded, e.g. `.../bin/dvc repro pdf2md`.
    pub command: String,
    /// The paths it holds a write lock on, as DVC recorded them (absolute).
    pub writes: Vec<String>,
}

impl ExternalRun {
    /// How long the process has been running (the age of its `/proc` entry).
    pub fn age(&self) -> Option<Duration> {
        fs::metadata(format!("/proc/{}", self.pid))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
    }

    /// Does this run touch the stage? Either it was asked for by name (`dvc repro <stage>`), or
    /// it holds a write lock on something the stage produces (`produced`, relative to `root`).
    pub fn works_on<'a>(
        &self,
        root: &Path,
        stage: &str,
        mut produced: impl Iterator<Item = &'a str>,
    ) -> bool {
        let named = self
            .command
            .split_whitespace()
            .skip_while(|word| *word != "repro")
            .skip(1)
            .any(|word| word == stage);
        named
            || produced.any(|path| {
                let full = root.join(path.trim_end_matches('/'));
                self.writes.iter().any(|held| {
                    let held = Path::new(held);
                    full.starts_with(held) || held.starts_with(&full)
                })
            })
    }

    /// A short form for messages: the command without the directory of the executable.
    pub fn short_command(&self) -> String {
        let mut words = self.command.split_whitespace();
        let program = words.next().unwrap_or_default();
        let program = program.rsplit('/').next().unwrap_or(program);
        std::iter::once(program)
            .chain(words)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Does this log say that the run was refused because of another DVC process?
pub fn log_mentions_lock(log: &str) -> bool {
    log.contains(BLOCKED_TEXT)
}

/// The processes named in the text of `.dvc/tmp/rwlock`, each once, with the paths they write.
pub fn iter_lock_holders(text: &str) -> impl Iterator<Item = ExternalRun> {
    let mut found: Vec<ExternalRun> = Vec::new();
    if let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) {
        for (kind, section) in &root {
            let Value::Object(files) = section else {
                continue;
            };
            for (file, holders) in files {
                // "read" maps a file to a list of holders, "write" to a single one.
                let list: Vec<&Value> = match holders {
                    Value::Array(items) => items.iter().collect(),
                    other => vec![other],
                };
                for holder in list {
                    let pid = holder.get("pid").and_then(Value::as_u64);
                    let command = holder.get("cmd").and_then(Value::as_str);
                    let (Some(pid), Some(command)) = (pid, command) else {
                        continue;
                    };
                    let Ok(pid) = u32::try_from(pid) else {
                        continue;
                    };
                    let index = match found.iter().position(|run| run.pid == pid) {
                        Some(index) => index,
                        None => {
                            found.push(ExternalRun {
                                pid,
                                command: command.to_owned(),
                                writes: Vec::new(),
                            });
                            found.len() - 1
                        }
                    };
                    if kind == "write" {
                        found[index].writes.push(file.clone());
                    }
                }
            }
        }
    }
    found.sort_by_key(|run| run.pid);
    found.into_iter()
}

/// Is `pid` a live process whose command line looks like `dvc`? The check protects against a
/// recycled process id: a stale entry in the lock file must never lead to an unrelated kill.
fn is_live_dvc(pid: u32) -> bool {
    let Ok(bytes) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let zombie = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit(')')
                .next()
                .map(|r| r.trim_start().starts_with('Z'))
        })
        .unwrap_or(false);
    !zombie
        && text
            .split('\0')
            .any(|word| word.rsplit('/').next() == Some("dvc"))
}

/// Was `pid` started by dvcat? Its runs go through a wrapper whose command line carries a marker.
fn started_by_dvcat(pid: u32) -> bool {
    let mut current = pid;
    for _ in 0..32 {
        let Ok(bytes) = fs::read(format!("/proc/{current}/cmdline")) else {
            return false;
        };
        if String::from_utf8_lossy(&bytes).contains(crate::runner::MARKER) {
            return true;
        }
        let parent = fs::read_to_string(format!("/proc/{current}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit(')')
                    .next()?
                    .split_whitespace()
                    .nth(1)?
                    .parse::<u32>()
                    .ok()
            });
        match parent {
            Some(parent) if parent > 1 => current = parent,
            _ => return false,
        }
    }
    false
}

/// The live `dvc` processes that hold the lock of the project in `root`, other than dvcat's own.
pub fn iter_external_runs(root: &Path) -> impl Iterator<Item = ExternalRun> {
    let text = fs::read_to_string(root.join(".dvc/tmp/rwlock")).unwrap_or_default();
    iter_lock_holders(&text)
        .filter(|holder| is_live_dvc(holder.pid) && !started_by_dvcat(holder.pid))
        .collect::<Vec<_>>()
        .into_iter()
}

/// `pid` and everything below it, deepest first, so that a stage command is stopped before the
/// `dvc` that started it.
fn tree_leaves_first(pid: u32) -> Vec<u32> {
    let mut parent_of = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(child) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let parent = fs::read_to_string(entry.path().join("stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit(')')
                        .next()?
                        .split_whitespace()
                        .nth(1)?
                        .parse::<u32>()
                        .ok()
                });
            if let Some(parent) = parent {
                parent_of.push((child, parent));
            }
        }
    }
    let mut order = vec![pid];
    let mut next = 0;
    while next < order.len() {
        let current = order[next];
        next += 1;
        for (child, parent) in &parent_of {
            if *parent == current && !order.contains(child) {
                order.push(*child);
            }
        }
    }
    order.reverse();
    order
}

fn signal(signal: &str, pid: u32) {
    let _ = Command::new("kill")
        .args(["-s", signal, "--"])
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Stop the processes of `runs` (and what they started): SIGTERM now, SIGKILL after `grace` for
/// whatever is still alive. Entries that are no longer a live `dvc` are left alone.
pub fn stop(runs: &[u32], grace: Duration) {
    let targets: Vec<u32> = runs
        .iter()
        .copied()
        .filter(|pid| is_live_dvc(*pid))
        .flat_map(tree_leaves_first)
        .collect();
    for pid in &targets {
        signal("TERM", *pid);
    }
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline
        && targets
            .iter()
            .any(|p| fs::metadata(format!("/proc/{p}")).is_ok())
    {
        thread::sleep(Duration::from_millis(50));
    }
    for pid in &targets {
        if fs::metadata(format!("/proc/{pid}")).is_ok() {
            signal("KILL", *pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"{"read": {"a.py": [{"pid": 7, "cmd": "/v/bin/dvc repro"}, {"pid": 9, "cmd": "/v/bin/dvc repro pdf2md"}],
        "b": [{"pid": 9, "cmd": "/v/bin/dvc repro pdf2md"}]},
        "write": {"out": {"pid": 9, "cmd": "/v/bin/dvc repro pdf2md"}}}"#;

    #[test]
    fn holders_are_listed_once_each() {
        let holders: Vec<_> = iter_lock_holders(LOCK).collect();
        assert_eq!(holders.iter().map(|h| h.pid).collect::<Vec<_>>(), [7, 9]);
        assert_eq!(holders[1].short_command(), "dvc repro pdf2md");
        assert_eq!(holders[1].writes, ["out"]);
        assert!(holders[0].writes.is_empty());
    }

    #[test]
    fn a_run_is_matched_to_stages_by_name_or_by_written_paths() {
        let run = ExternalRun {
            pid: 1,
            command: "/v/bin/dvc repro -f pdf2md".into(),
            writes: vec!["/p/data/md_files".into()],
        };
        let root = Path::new("/p");
        assert!(run.works_on(root, "pdf2md", std::iter::empty()));
        assert!(run.works_on(root, "other", ["data/md_files/"].into_iter()));
        assert!(run.works_on(root, "other", ["data"].into_iter()));
        assert!(!run.works_on(root, "other", ["data/layout"].into_iter()));
        assert!(!run.works_on(root, "other", ["pdf2md_errors.jsonl"].into_iter()));
    }

    #[test]
    fn a_damaged_lock_file_has_no_holders() {
        assert_eq!(iter_lock_holders("not json").count(), 0);
        assert_eq!(iter_lock_holders("").count(), 0);
    }

    #[test]
    fn dvc_lock_messages_are_recognised() {
        assert!(log_mentions_lock(
            "ERROR: 'x' is busy, it is being blocked by:\n (PID 1)"
        ));
        assert!(!log_mentions_lock("ERROR: stage failed"));
    }

    #[test]
    fn only_live_dvc_processes_count() {
        assert!(!is_live_dvc(u32::MAX));
        assert!(!is_live_dvc(std::process::id())); // alive, but not dvc
    }

    #[test]
    fn stopping_ends_a_live_dvc_and_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = dir.path().join("dvc");
        fs::write(&dvc, "#!/bin/sh\nsleep 60 &\nwait\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dvc, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(&dvc).spawn().unwrap();
        let pid = child.id();
        for _ in 0..100 {
            if is_live_dvc(pid) && tree_leaves_first(pid).len() > 1 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let family = tree_leaves_first(pid);
        assert!(family.len() > 1, "expected the sleep child: {family:?}");
        stop(&[pid], Duration::from_secs(2));
        let _ = child.wait();
        for p in family {
            let gone = fs::read_to_string(format!("/proc/{p}/stat")).map_or(true, |s| {
                s.rsplit(')')
                    .next()
                    .is_some_and(|r| r.trim_start().starts_with('Z'))
            });
            assert!(gone, "{p} still running");
        }
    }
}
