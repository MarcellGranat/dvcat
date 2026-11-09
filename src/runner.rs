//! Running `dvc` as a child process: locating it, streaming its output, and stopping it.

use std::{
    env,
    ffi::OsStr,
    fmt,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::app::Event;

/// Highest-priority place to look for `dvc`.
pub const DVC_ENV_VAR: &str = "DVCAT_DVC";
/// How long a cancelled run may take to react to SIGTERM before it is killed.
pub const KILL_GRACE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// How often the log file is read for new output.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(40);
/// Attempts (5 ms apart) to find the marker in a process's command line.
const CMDLINE_RETRIES: usize = 60;
/// How often a run without an exit status is checked for being alive.
const ALIVE_CHECK_INTERVAL: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------------------------
// Locating dvc
// ---------------------------------------------------------------------------------------------

/// `dvc` was not found; `searched` lists every place we looked, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DvcNotFound {
    pub searched: Vec<String>,
}

impl fmt::Display for DvcNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One place per line: the log pane clips long lines instead of wrapping them.
        write!(f, "dvc not found. Looked in:")?;
        for place in &self.searched {
            write!(f, "\n  {place}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DvcNotFound {}

/// Order: `$DVCAT_DVC`, `<project>/.venv/bin/dvc`, then every directory on `PATH`.
/// The environment is passed in (not read here) so the order can be tested.
pub fn find_dvc(
    project_root: &Path,
    env_override: Option<&OsStr>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, DvcNotFound> {
    let mut searched = Vec::new();

    if let Some(configured) = env_override {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Ok(path);
        }
        searched.push(format!("${DVC_ENV_VAR}={}", path.display()));
    }

    let venv = project_root.join(".venv/bin/dvc");
    if venv.is_file() {
        return Ok(venv);
    }
    searched.push(venv.display().to_string());

    let directories: Vec<PathBuf> = path_var
        .map(|p| env::split_paths(p).collect())
        .unwrap_or_default();
    if let Some(found) = directories
        .iter()
        .map(|d| d.join("dvc"))
        .find(|c| c.is_file())
    {
        return Ok(found);
    }
    searched.push(format!("PATH ({} directories)", directories.len()));

    Err(DvcNotFound { searched })
}

/// [`find_dvc`] using the real environment.
pub fn find_dvc_in_env(project_root: &Path) -> Result<PathBuf, DvcNotFound> {
    find_dvc(
        project_root,
        env::var_os(DVC_ENV_VAR).as_deref(),
        env::var_os("PATH").as_deref(),
    )
}

/// `r` runs the stage alone; `R` also runs stale stages upstream of it (DVC's default).
pub fn repro_args(stage: &str, with_upstream: bool) -> Vec<String> {
    let mut args = vec!["repro".to_owned()];
    if !with_upstream {
        args.push("--single-item".to_owned());
    }
    args.push(stage.to_owned());
    args
}

/// `dvc exp run --temp [-n NAME] -S file:key=value ...`.
/// `--temp` runs in an isolated copy of the workspace, so experiments do not inherit each
/// other's params and the workspace stays as it is.
pub fn exp_run_args(name: Option<&str>, overrides: &[(String, String)]) -> Vec<String> {
    let mut args: Vec<String> = ["exp", "run", "--temp"].map(String::from).into();
    if let Some(name) = name {
        args.extend(["-n".to_owned(), name.to_owned()]);
    }
    for (key, value) in overrides {
        args.extend(["-S".to_owned(), format!("{key}={value}")]);
    }
    args
}

/// DVC saves the previous workspace to `refs/exps/apply/stash` before overwriting it.
/// (`--no-force` is deprecated since DVC 3 and no longer protects anything.)
pub fn exp_apply_args(experiment: &str) -> Vec<String> {
    ["exp", "apply", experiment].map(String::from).into()
}

/// Promote an experiment to a Git branch (not checked out).
pub fn exp_branch_args(experiment: &str, branch: &str) -> Vec<String> {
    ["exp", "branch", experiment, branch]
        .map(String::from)
        .into()
}

pub fn exp_remove_args(experiment: &str) -> Vec<String> {
    ["exp", "remove", experiment].map(String::from).into()
}

// ---------------------------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Succeeded,
    /// `code` is `None` when the process was ended by a signal.
    Failed {
        code: Option<i32>,
    },
    Cancelled,
    /// The process never started (for example `dvc` was not found).
    NotStarted(String),
}

/// What is remembered about a run so that a later session can pick it up again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Process group id of the run's wrapper (`0` until it has been spawned).
    pub pgid: u32,
    /// The `dvc` arguments.
    pub args: Vec<String>,
    /// A stage name, or a label such as `exp lr-1`.
    pub stage: String,
    /// `stage`, `experiment` or `branch`.
    pub kind: String,
    /// The branch name, for `kind == "branch"`.
    pub branch: Option<String>,
    pub with_upstream: bool,
    /// When the run was started, in milliseconds since the Unix epoch.
    pub started_ms: u64,
}

impl RunRecord {
    /// A record for a plain `dvc <args>` run.
    pub fn plain(args: &[String]) -> Self {
        Self {
            pgid: 0,
            args: args.to_vec(),
            stage: String::new(),
            kind: "stage".to_owned(),
            branch: None,
            with_upstream: false,
            started_ms: now_ms(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Handle to a running `dvc` process and everything it spawned. Dropping the handle does **not**
/// stop the run: runs outlive the interface on purpose (see [`attach`]).
#[derive(Debug)]
pub struct RunningJob {
    /// Process group id: the wrapper leads its own group, so this equals its pid.
    pgid: u32,
    /// Set once the run is over (its exit status was read, or it is known to be gone).
    finished: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    /// Started by this session, so the process id is known to be ours. A run found from an earlier
    /// session is not trusted: its id is checked before it is ever signalled.
    trusted: bool,
}

// ---------------------------------------------------------------------------------------------
// Where a run keeps its state
// ---------------------------------------------------------------------------------------------

const RECORD_FILE: &str = "dvcat-run.json";
const LOG_FILE: &str = "dvcat-run.log";
const EXIT_FILE: &str = "dvcat-run.exit";
/// Appears in the wrapper's command line, which is how a live run is recognised (and a recycled
/// process id is not mistaken for it).
pub(crate) const MARKER: &str = "dvcat_run";
/// Runs `"$@"` with its output appended to a file and writes the exit status to another, so both
/// outlive the interface. `trap :` keeps the wrapper alive when the group gets SIGTERM, so it can
/// still record the status (a handler, unlike "ignore", is reset by `exec` in the children).
/// Exit status 126 is retried: a freshly written script can briefly look "busy" to `exec`.
const WRAPPER: &str = r#": dvcat_run
trap : TERM INT
tries=0
while :; do
  "$@" >>"$DVCAT_LOG" 2>&1 </dev/null
  code=$?
  if [ "$code" = 126 ] && [ "$tries" -lt 5 ]; then tries=$((tries+1)); sleep 0.02; continue; fi
  break
done
echo "$code" >"$DVCAT_EXIT""#;

/// `<root>/.dvc/tmp` (DVC keeps it out of Git), or `<root>/.dvcat` when there is no `.dvc`.
fn state_dir(root: &Path) -> PathBuf {
    let dvc = root.join(".dvc");
    if dvc.is_dir() {
        dvc.join("tmp")
    } else {
        root.join(".dvcat")
    }
}

/// Delete the files of the last run (once it has been shown, or before a new one starts).
pub fn forget(root: &Path) {
    let dir = state_dir(root);
    for file in [RECORD_FILE, LOG_FILE, EXIT_FILE] {
        let _ = fs::remove_file(dir.join(file));
    }
}

/// Start `dvc <args>` in `root`. Output and the final outcome arrive on `tx` as events.
pub fn start(
    dvc: &Path,
    root: &Path,
    args: &[String],
    tx: Sender<Event>,
) -> io::Result<RunningJob> {
    start_recorded(dvc, root, RunRecord::plain(args), tx)
}

/// Start the run described by `record`, and write the record next to the run's log so that a later
/// session can [`attach`] to it. The process is detached from this one: it keeps going if the
/// interface quits.
pub fn start_recorded(
    dvc: &Path,
    root: &Path,
    mut record: RunRecord,
    tx: Sender<Event>,
) -> io::Result<RunningJob> {
    // A path we were given must exist; a bare name is looked up through PATH by the shell.
    if dvc.components().count() > 1 {
        fs::metadata(dvc)?;
    }
    let dir = state_dir(root);
    fs::create_dir_all(&dir)?;
    forget(root);
    File::create(dir.join(LOG_FILE))?;

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(WRAPPER)
        .arg("sh")
        .arg(dvc)
        .args(&record.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("DVCAT_LOG", dir.join(LOG_FILE))
        .env("DVCAT_EXIT", dir.join(EXIT_FILE))
        // A Python stdout that is not a terminal is block-buffered; without this, output appears
        // only at exit.
        .env("PYTHONUNBUFFERED", "1")
        .env("NO_COLOR", "1")
        // Lead a new process group, so one signal can reach dvc *and* the stage command it starts.
        .process_group(0)
        .spawn()?;
    let pgid = child.id();
    record.pgid = pgid;
    // Not fatal: without the record the run works, it just cannot be picked up again.
    if let Ok(text) = serde_json::to_string_pretty(&record) {
        let _ = fs::write(dir.join(RECORD_FILE), text);
    }
    // Reap the wrapper when it ends, so it never lingers as a zombie while we are still running,
    // and remember that it ended: for our own child that is the reliable "it is gone" signal.
    let exited = Arc::new(AtomicBool::new(false));
    let exited_flag = Arc::clone(&exited);
    thread::spawn(move || {
        let _ = child.wait();
        exited_flag.store(true, Ordering::SeqCst);
    });

    Ok(follow(&dir, pgid, tx, Some(exited)))
}

/// Follow the run in `dir`: forward new log output and, when the exit status appears (or the run
/// is found dead), announce the outcome.
/// `exited` is set by whoever reaps the process when it is our own child; for a run from an
/// earlier session it is `None` and liveness is judged from `/proc`.
fn follow(dir: &Path, pgid: u32, tx: Sender<Event>, exited: Option<Arc<AtomicBool>>) -> RunningJob {
    let trusted = exited.is_some();
    let finished = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (finished_flag, cancelled_flag) = (Arc::clone(&finished), Arc::clone(&cancelled));
    let (log_path, exit_path) = (dir.join(LOG_FILE), dir.join(EXIT_FILE));

    thread::spawn(move || {
        let mut tail = Tail::new(&log_path);
        let mut last_alive_check = Instant::now();
        loop {
            if !tail.forward(&tx) {
                return; // nobody is listening any more
            }
            let status = fs::read_to_string(&exit_path)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok());
            // Without an exit status, check now and then that the run is still there.
            let mut gone = false;
            if status.is_none() && last_alive_check.elapsed() >= ALIVE_CHECK_INTERVAL {
                last_alive_check = Instant::now();
                gone = match &exited {
                    Some(flag) => flag.load(Ordering::SeqCst),
                    None => !group_is_ours(pgid),
                };
            }
            if status.is_some() || gone {
                // The exit file may be written just as the group dies: look once more.
                let status = status.or_else(|| {
                    fs::read_to_string(&exit_path)
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                });
                finished_flag.store(true, Ordering::SeqCst);
                if !tail.forward(&tx) {
                    return;
                }
                tail.finish(&tx);
                let outcome = match status {
                    _ if cancelled_flag.load(Ordering::SeqCst) => RunOutcome::Cancelled,
                    Some(0) => RunOutcome::Succeeded,
                    code => RunOutcome::Failed { code },
                };
                let _ = tx.send(Event::RunFinished(outcome));
                return;
            }
            thread::sleep(FOLLOW_INTERVAL);
        }
    });

    RunningJob {
        pgid,
        finished,
        cancelled,
        trusted,
    }
}

/// Reads a growing log file, sending what is new as `Event::RunOutput`.
struct Tail {
    path: PathBuf,
    offset: u64,
    /// Bytes of a UTF-8 character cut in half by the end of the file.
    pending: Vec<u8>,
}

impl Tail {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            offset: 0,
            pending: Vec::new(),
        }
    }

    /// Send everything appended since the last call. `false` when the receiver is gone.
    fn forward(&mut self, tx: &Sender<Event>) -> bool {
        let Ok(mut file) = File::open(&self.path) else {
            return true;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return true;
        }
        let mut buffer = Vec::new();
        let Ok(read) = file.read_to_end(&mut buffer) else {
            return true;
        };
        self.offset += read as u64;
        self.pending.extend_from_slice(&buffer);
        let text = drain_utf8(&mut self.pending);
        text.is_empty() || tx.send(Event::RunOutput(text)).is_ok()
    }

    /// Flush a trailing half character as `�` once nothing more will come.
    fn finish(&mut self, tx: &Sender<Event>) {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            let _ = tx.send(Event::RunOutput(text));
        }
    }
}

/// A run from an earlier session, found on startup.
#[derive(Debug)]
pub enum Attached {
    /// Still going: its output so far and from now on arrive as events.
    Running { record: RunRecord, job: RunningJob },
    /// It ended while the interface was closed (or died without leaving a status).
    Finished {
        record: RunRecord,
        log: String,
        outcome: RunOutcome,
        /// How long it ran, when that can be told.
        duration: Option<Duration>,
    },
}

/// Pick up the run an earlier session left behind in `root`, if any. A finished run is read in
/// full and its files are removed (so it is reported once); a live one is followed from the start
/// of its log.
pub fn attach(root: &Path, tx: Sender<Event>) -> Option<Attached> {
    let dir = state_dir(root);
    let record: RunRecord =
        serde_json::from_str(&fs::read_to_string(dir.join(RECORD_FILE)).ok()?).ok()?;
    let exit_path = dir.join(EXIT_FILE);
    let status = fs::read_to_string(&exit_path)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok());

    if status.is_none() && group_is_ours(record.pgid) {
        return Some(Attached::Running {
            job: follow(&dir, record.pgid, tx, None),
            record,
        });
    }

    let log = fs::read(dir.join(LOG_FILE))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    // The exit file is written when the run ends; failing that, the log's last write is the best guess.
    let ended = fs::metadata(&exit_path)
        .or_else(|_| fs::metadata(dir.join(LOG_FILE)))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    let duration =
        ended.map(|ended| Duration::from_millis(ended.saturating_sub(record.started_ms)));
    let outcome = match status {
        Some(0) => RunOutcome::Succeeded,
        code => RunOutcome::Failed { code },
    };
    forget(root);
    Some(Attached::Finished {
        record,
        log,
        outcome,
        duration,
    })
}

/// Is `pgid` the wrapper of one of our runs? The process id alone is not enough: after a reboot
/// the number may belong to something unrelated, which must never be signalled.
fn group_is_ours(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    if Path::new("/proc/self").exists() {
        // Just after `exec` the kernel can show an empty (or still the parent's) command line for
        // a moment, so a reading without the marker is retried unless the process is already gone.
        for _ in 0..CMDLINE_RETRIES {
            match fs::read(format!("/proc/{pgid}/cmdline")) {
                Ok(bytes) if String::from_utf8_lossy(&bytes).contains(MARKER) => return true,
                Ok(_) if is_zombie(pgid) => return false,
                Ok(_) => thread::sleep(Duration::from_millis(5)),
                Err(_) => return false,
            }
        }
        return false;
    }
    // No /proc (not Linux): the best we can do is ask whether the group exists.
    Command::new("kill")
        .args(["-0", "--"])
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Has the process exited without having been reaped yet? (`/proc/<pid>/stat` shows state `Z`.)
fn is_zombie(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit(')')
                .next()
                .map(|rest| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Take the valid UTF-8 from the front of `pending`. A multi-byte character cut in half by a
/// read boundary stays in `pending` for the next call; genuinely invalid bytes become `�`.
fn drain_utf8(pending: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                out.push_str(valid);
                pending.clear();
                return out;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                out.push_str(
                    std::str::from_utf8(&pending[..valid_up_to]).expect("prefix is valid"),
                );
                match error.error_len() {
                    Some(bad_bytes) => {
                        out.push('\u{FFFD}');
                        pending.drain(..valid_up_to + bad_bytes);
                    }
                    None => {
                        pending.drain(..valid_up_to);
                        return out;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Stopping
// ---------------------------------------------------------------------------------------------

/// Send a signal to a whole process group. The standard library cannot do this, and the `kill`
/// program can, without any `unsafe` code. Errors are ignored: the group may already be gone.
fn signal_group(pgid: u32, signal: &str, trusted: bool) {
    // Never signal a group that is not one of our runs (a recycled process id, for example).
    if !trusted && !group_is_ours(pgid) {
        return;
    }
    let _ = Command::new("kill")
        .args(["-s", signal, "--"])
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl RunningJob {
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Ask the run to stop: SIGTERM now, SIGKILL after [`KILL_GRACE`] if it is still alive.
    /// Returns immediately; `Event::RunFinished(Cancelled)` follows when the process is gone.
    pub fn cancel(&self) {
        self.cancel_with_grace(KILL_GRACE);
    }

    pub fn cancel_with_grace(&self, grace: Duration) {
        if self.is_finished() || self.cancelled.swap(true, Ordering::SeqCst) {
            return; // nothing to stop, or already asked
        }
        signal_group(self.pgid, "TERM", self.trusted);

        let (pgid, finished, trusted) = (self.pgid, Arc::clone(&self.finished), self.trusted);
        thread::spawn(move || {
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                if finished.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(POLL_INTERVAL);
            }
            if !finished.load(Ordering::SeqCst) {
                signal_group(pgid, "KILL", trusted);
            }
        });
    }

    /// Cancel and wait (up to `timeout`) until the process has really exited.
    pub fn terminate_blocking(&self, grace: Duration, timeout: Duration) -> bool {
        self.cancel_with_grace(grace);
        let deadline = Instant::now() + timeout;
        while !self.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        self.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, sync::mpsc, time::Duration};

    use super::*;
    use crate::log::LogBuffer;

    // ---- 6.1: finding dvc ----

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn env_override_wins_over_venv_and_path() {
        let dir = tempfile::tempdir().unwrap();
        let (from_env, venv, on_path) = (
            dir.path().join("custom/dvc"),
            dir.path().join("proj/.venv/bin/dvc"),
            dir.path().join("bin/dvc"),
        );
        for p in [&from_env, &venv, &on_path] {
            executable(p);
        }
        let path_var = on_path.parent().unwrap().as_os_str().to_owned();
        let found = find_dvc(
            &dir.path().join("proj"),
            Some(from_env.as_os_str()),
            Some(&path_var),
        );
        assert_eq!(found, Ok(from_env));
    }

    #[test]
    fn venv_beats_path() {
        let dir = tempfile::tempdir().unwrap();
        let (venv, on_path) = (
            dir.path().join("proj/.venv/bin/dvc"),
            dir.path().join("bin/dvc"),
        );
        executable(&venv);
        executable(&on_path);
        let path_var = on_path.parent().unwrap().as_os_str().to_owned();
        assert_eq!(
            find_dvc(&dir.path().join("proj"), None, Some(&path_var)),
            Ok(venv)
        );
    }

    #[test]
    fn falls_back_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = dir.path().join("bin/dvc");
        executable(&on_path);
        let path_var = env::join_paths([dir.path().join("empty"), dir.path().join("bin")]).unwrap();
        assert_eq!(find_dvc(dir.path(), None, Some(&path_var)), Ok(on_path));
    }

    #[test]
    fn not_found_message_has_one_place_per_line() {
        let error = DvcNotFound {
            searched: vec!["a".into(), "b".into()],
        };
        assert_eq!(error.to_string(), "dvc not found. Looked in:\n  a\n  b");
    }

    #[test]
    fn not_found_lists_every_place_searched() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope/dvc");
        let path_var = env::join_paths([dir.path().join("a"), dir.path().join("b")]).unwrap();
        let error = find_dvc(dir.path(), Some(missing.as_os_str()), Some(&path_var)).unwrap_err();
        assert_eq!(error.searched.len(), 3, "{error}");
        let message = error.to_string();
        assert!(message.contains("DVCAT_DVC"), "{message}");
        assert!(message.contains(".venv/bin/dvc"), "{message}");
        assert!(message.contains("PATH (2 directories)"), "{message}");
    }

    #[test]
    fn missing_path_variable_still_reports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let error = find_dvc(dir.path(), None, None).unwrap_err();
        assert!(error.to_string().contains("PATH (0 directories)"));
    }

    #[test]
    fn exp_run_args_name_and_override_params() {
        assert_eq!(exp_run_args(None, &[]), ["exp", "run", "--temp"]);
        let overrides = [("params.yaml:train.lr".to_owned(), "0.2".to_owned())];
        assert_eq!(
            exp_run_args(Some("lr-020"), &overrides),
            [
                "exp",
                "run",
                "--temp",
                "-n",
                "lr-020",
                "-S",
                "params.yaml:train.lr=0.2"
            ]
        );
    }

    #[test]
    fn branch_args_name_the_experiment_and_the_branch() {
        assert_eq!(
            exp_branch_args("lr-005", "best"),
            ["exp", "branch", "lr-005", "best"]
        );
    }

    #[test]
    fn apply_and_remove_args_name_the_experiment() {
        assert_eq!(exp_apply_args("lr-005"), ["exp", "apply", "lr-005"]);
        assert_eq!(exp_remove_args("lr-005"), ["exp", "remove", "lr-005"]);
    }

    #[test]
    fn repro_args_pick_the_right_flags() {
        assert_eq!(
            repro_args("train", false),
            ["repro", "--single-item", "train"]
        );
        assert_eq!(repro_args("train", true), ["repro", "train"]);
    }

    // ---- 6.2: running ----

    /// A stand-in for `dvc`: a shell script with the given body.
    fn fake_dvc(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-dvc");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Run to completion; return the log the app would show and the outcome.
    fn run_to_end(job_rx: &mpsc::Receiver<Event>) -> (Vec<String>, RunOutcome) {
        let mut log = LogBuffer::new();
        loop {
            match job_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("run did not finish")
            {
                Event::RunOutput(text) => log.push_str(&text),
                Event::RunFinished(outcome) => {
                    log.finish();
                    return (log.lines().map(str::to_owned).collect(), outcome);
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn streams_stdout_and_stderr_and_reports_success() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo \"args: $*\"; echo oops >&2; pwd");
        let (tx, rx) = mpsc::channel();
        let _job = start(&dvc, dir.path(), &args(&["repro", "x"]), tx).unwrap();
        let (lines, outcome) = run_to_end(&rx);
        assert_eq!(outcome, RunOutcome::Succeeded);
        assert!(lines.contains(&"args: repro x".to_owned()), "{lines:?}");
        assert!(lines.contains(&"oops".to_owned()), "{lines:?}");
        // The command ran inside the project directory.
        let cwd = dir.path().canonicalize().unwrap().display().to_string();
        assert!(lines.contains(&cwd), "{lines:?}");
    }

    #[test]
    fn carriage_return_progress_shows_only_the_latest_state() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(
            dir.path(),
            r"printf 'start\nprogress 20%%\rprogress 60%%\rprogress 100%%\nend\n'",
        );
        let (tx, rx) = mpsc::channel();
        let _job = start(&dvc, dir.path(), &[], tx).unwrap();
        let (lines, _) = run_to_end(&rx);
        assert_eq!(lines, ["start", "progress 100%", "end"]);
    }

    #[test]
    fn non_zero_exit_is_reported_with_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo failing; exit 3");
        let (tx, rx) = mpsc::channel();
        let _job = start(&dvc, dir.path(), &[], tx).unwrap();
        let (lines, outcome) = run_to_end(&rx);
        assert_eq!(outcome, RunOutcome::Failed { code: Some(3) });
        assert_eq!(lines, ["failing"]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn output_is_delivered_while_the_process_is_still_running() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo first; sleep 2; echo second");
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let first = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("live output");
        assert!(
            matches!(&first, Event::RunOutput(t) if t.contains("first")),
            "{first:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(!job.is_finished());
        run_to_end(&rx);
    }

    #[test]
    fn starting_a_missing_program_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let error = start(&dir.path().join("no-such-dvc"), dir.path(), &[], tx).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn multibyte_characters_split_across_reads_survive() {
        let mut pending = vec![0xE2, 0x9C]; // first two bytes of "✓" (E2 9C 93)
        assert_eq!(drain_utf8(&mut pending), "");
        pending.push(0x93);
        assert_eq!(drain_utf8(&mut pending), "✓");
        assert!(pending.is_empty());

        let mut invalid = b"ok\xFFmore".to_vec();
        assert_eq!(drain_utf8(&mut invalid), "ok\u{FFFD}more");
    }

    // ---- cancelling ----

    /// Is this process gone? A zombie that nobody has reaped yet counts as gone.
    fn process_is_gone(pid: u32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z')),
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_for_file(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(text) = fs::read_to_string(path)
                && !text.trim().is_empty()
            {
                return text;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("{} never appeared", path.display());
    }

    fn wait_until_gone(pid: u32) -> bool {
        (0..100).any(|_| {
            thread::sleep(Duration::from_millis(20));
            process_is_gone(pid)
        })
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cancel_stops_the_whole_process_group_including_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        // The script starts a background grandchild and records its pid, like dvc does with a stage command.
        let dvc = fake_dvc(dir.path(), "sleep 60 &\necho $! > grandchild.pid\nwait");
        let (tx, rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let grandchild: u32 = wait_for_file(&dir.path().join("grandchild.pid"))
            .trim()
            .parse()
            .unwrap();
        assert!(!process_is_gone(grandchild));

        job.cancel();
        let (_, outcome) = run_to_end(&rx);
        assert_eq!(outcome, RunOutcome::Cancelled);
        assert!(
            wait_until_gone(grandchild),
            "grandchild {grandchild} survived the cancel"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_process_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(
            dir.path(),
            "trap '' TERM\nsleep 60 &\necho $! > grandchild.pid\nwait",
        );
        let (tx, rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let grandchild: u32 = wait_for_file(&dir.path().join("grandchild.pid"))
            .trim()
            .parse()
            .unwrap();

        let started = Instant::now();
        job.cancel_with_grace(Duration::from_millis(300));
        let (_, outcome) = run_to_end(&rx);
        assert_eq!(outcome, RunOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(wait_until_gone(grandchild));
    }

    #[test]
    fn terminate_blocking_returns_only_once_the_process_has_exited() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "sleep 60");
        let (tx, _rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        assert!(job.terminate_blocking(Duration::from_millis(300), Duration::from_secs(5)));
        assert!(job.is_finished());
    }

    #[test]
    fn cancelling_a_finished_run_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "true");
        let (tx, rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let (_, outcome) = run_to_end(&rx);
        assert_eq!(outcome, RunOutcome::Succeeded);
        job.cancel(); // must not signal a pid that may have been reused
        assert!(job.is_finished());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn dropping_a_running_job_leaves_the_process_running() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "sleep 60 &\necho $! > grandchild.pid\nwait");
        let (tx, _rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let pgid = job.pgid;
        let grandchild: u32 = wait_for_file(&dir.path().join("grandchild.pid"))
            .trim()
            .parse()
            .unwrap();
        drop(job);
        thread::sleep(Duration::from_millis(300));
        assert!(
            !process_is_gone(grandchild),
            "runs must outlive the interface"
        );
        signal_group(pgid, "KILL", true); // clean up
        assert!(wait_until_gone(grandchild));
    }

    // ---- runs that outlive the interface ----

    fn wait_for_exit_file(root: &Path) {
        let path = state_dir(root).join(EXIT_FILE);
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("the run never finished");
    }

    fn collect(rx: &mpsc::Receiver<Event>) -> (String, RunOutcome) {
        let mut text = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("event") {
                Event::RunOutput(chunk) => text.push_str(&chunk),
                Event::RunFinished(outcome) => return (text, outcome),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn recorded(args: &[&str]) -> RunRecord {
        RunRecord {
            stage: "train".into(),
            kind: "stage".into(),
            ..RunRecord::plain(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
        }
    }

    #[test]
    fn a_later_session_follows_a_run_that_is_still_going() {
        let dir = tempfile::tempdir().unwrap();
        // The run waits for a gate file, so it is certainly still going when the next session looks.
        let dvc = fake_dvc(
            dir.path(),
            "echo first\nwhile [ ! -f go ]; do sleep 0.02; done\necho second",
        );
        let (tx, rx) = mpsc::channel();
        let job = start_recorded(&dvc, dir.path(), recorded(&["repro", "train"]), tx).unwrap();
        // "Quit": the first session goes away without stopping the run.
        drop(job);
        drop(rx);

        let (tx2, rx2) = mpsc::channel();
        let attached = attach(dir.path(), tx2);
        let Some(Attached::Running { record, job: _job }) = attached else {
            panic!("the run should still be going, got {attached:?}");
        };
        assert_eq!(record.stage, "train");
        assert_eq!(record.args, ["repro", "train"]);
        fs::write(dir.path().join("go"), "").unwrap();
        let (text, outcome) = collect(&rx2);
        assert!(
            text.contains("first") && text.contains("second"),
            "{text:?}: earlier output is replayed, later output follows"
        );
        assert_eq!(outcome, RunOutcome::Succeeded);
    }

    #[test]
    fn a_run_that_finished_while_closed_is_reported_once_with_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo all done\nexit 3");
        let (tx, _rx) = mpsc::channel();
        let job = start_recorded(&dvc, dir.path(), recorded(&["repro"]), tx).unwrap();
        wait_for_exit_file(dir.path());
        drop(job);

        let (tx2, _rx2) = mpsc::channel();
        let Some(Attached::Finished {
            record,
            log,
            outcome,
            duration,
        }) = attach(dir.path(), tx2.clone())
        else {
            panic!("expected a finished run");
        };
        assert_eq!(record.stage, "train");
        assert!(log.contains("all done"), "{log:?}");
        assert_eq!(outcome, RunOutcome::Failed { code: Some(3) });
        assert!(duration.is_some());
        assert!(attach(dir.path(), tx2).is_none(), "reported only once");
    }

    #[test]
    fn a_run_that_died_without_a_status_is_reported_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "sleep 60");
        let (tx, _rx) = mpsc::channel();
        let job = start_recorded(&dvc, dir.path(), recorded(&["repro"]), tx).unwrap();
        let pgid = job.pgid;
        drop(job);
        signal_group(pgid, "KILL", true);
        assert!(wait_until_gone(pgid));

        let (tx2, _rx2) = mpsc::channel();
        let Some(Attached::Finished { outcome, .. }) = attach(dir.path(), tx2) else {
            panic!("expected a finished run");
        };
        assert_eq!(outcome, RunOutcome::Failed { code: None });
    }

    #[test]
    fn nothing_to_attach_to_is_none_and_a_damaged_record_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        assert!(attach(dir.path(), tx.clone()).is_none());
        fs::create_dir_all(state_dir(dir.path())).unwrap();
        fs::write(state_dir(dir.path()).join(RECORD_FILE), "{ broken").unwrap();
        assert!(attach(dir.path(), tx).is_none());
    }

    #[test]
    fn a_recycled_process_id_is_never_followed_or_signalled() {
        let dir = tempfile::tempdir().unwrap();
        // Some unrelated process whose id a stale record happens to name.
        let mut stranger = Command::new("sleep").arg("30").spawn().unwrap();
        let mut record = recorded(&["repro"]);
        record.pgid = stranger.id();
        fs::create_dir_all(state_dir(dir.path())).unwrap();
        fs::write(
            state_dir(dir.path()).join(RECORD_FILE),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();

        let (tx, _rx) = mpsc::channel();
        let attached = attach(dir.path(), tx);
        assert!(
            matches!(attached, Some(Attached::Finished { .. })),
            "{attached:?}"
        );
        signal_group(stranger.id(), "KILL", false); // refused: not one of our runs
        thread::sleep(Duration::from_millis(100));
        assert!(
            stranger.try_wait().unwrap().is_none(),
            "the stranger must still be alive"
        );
        stranger.kill().unwrap();
        stranger.wait().unwrap();
    }

    #[test]
    fn a_followed_run_can_be_cancelled_from_the_later_session() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "sleep 60");
        let (tx, _rx) = mpsc::channel();
        drop(start_recorded(&dvc, dir.path(), recorded(&["repro"]), tx).unwrap());

        let (tx2, rx2) = mpsc::channel();
        let Some(Attached::Running { job, .. }) = attach(dir.path(), tx2) else {
            panic!("expected a live run");
        };
        job.cancel();
        let (_, outcome) = collect(&rx2);
        assert_eq!(outcome, RunOutcome::Cancelled);
    }

    #[test]
    fn starting_a_new_run_clears_the_files_of_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "echo one");
        let (tx, rx) = mpsc::channel();
        start(&dvc, dir.path(), &[], tx.clone()).unwrap();
        collect(&rx);
        fs::write(&dvc, "#!/bin/sh\necho two\n").unwrap();
        start(&dvc, dir.path(), &[], tx).unwrap();
        let (text, _) = collect(&rx);
        assert!(text.contains("two") && !text.contains("one"), "{text:?}");
    }

    #[test]
    fn cancelling_right_after_starting_works_even_while_the_process_is_still_starting() {
        // The kernel can take a while to finish `exec` on a busy machine; the run is still ours.
        for _ in 0..25 {
            let dir = tempfile::tempdir().unwrap();
            let dvc = fake_dvc(dir.path(), "sleep 60");
            let (tx, rx) = mpsc::channel();
            let job = start(&dvc, dir.path(), &[], tx).unwrap();
            job.cancel();
            let (_, outcome) = collect(&rx);
            assert_eq!(outcome, RunOutcome::Cancelled);
        }
    }

    #[test]
    fn a_run_we_started_is_never_mistaken_for_gone_while_it_is_slow_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "while [ ! -f go ]; do sleep 0.02; done");
        let (tx, rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        // Longer than the interval between liveness checks: a wrong "gone" verdict would show here.
        assert!(
            rx.recv_timeout(Duration::from_millis(1100)).is_err(),
            "no end-of-run event while it runs"
        );
        fs::write(dir.path().join("go"), "").unwrap();
        let (_, outcome) = collect(&rx);
        assert_eq!(outcome, RunOutcome::Succeeded);
        drop(job);
    }
}
