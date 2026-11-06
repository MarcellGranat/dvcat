//! Running `dvc` as a child process: locating it, streaming its output, and stopping it.

use std::{
    env,
    ffi::OsStr,
    fmt,
    io::{self, Read},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::app::Event;

/// Highest-priority place to look for `dvc`.
pub const DVC_ENV_VAR: &str = "DVC_TUI_DVC";
/// How long a cancelled run may take to react to SIGTERM before it is killed.
pub const KILL_GRACE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SPAWN_RETRIES: usize = 5;

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
        write!(f, "dvc not found. Looked in: {}", self.searched.join("; "))
    }
}

impl std::error::Error for DvcNotFound {}

/// Order: `$DVC_TUI_DVC`, `<project>/.venv/bin/dvc`, then every directory on `PATH`.
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

/// Handle to a running `dvc` process and everything it spawned.
#[derive(Debug)]
pub struct RunningJob {
    /// Process group id: the child leads its own group, so this equals the child's pid.
    pgid: u32,
    /// Set by the waiter thread once the child has exited.
    finished: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

/// Start `dvc <args>` in `root`. Output and the final outcome arrive on `tx` as events.
pub fn start(
    dvc: &Path,
    root: &Path,
    args: &[String],
    tx: Sender<Event>,
) -> io::Result<RunningJob> {
    let mut child = spawn_with_retry(dvc, root, args)?;
    let pgid = child.id();
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let readers: Vec<JoinHandle<()>> = vec![
        spawn_pump(stdout, tx.clone()),
        spawn_pump(stderr, tx.clone()),
    ];

    let finished = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (finished_flag, cancelled_flag) = (Arc::clone(&finished), Arc::clone(&cancelled));
    thread::spawn(move || {
        let status = child.wait();
        // The process is gone: from now on its pid must never be signalled.
        finished_flag.store(true, Ordering::SeqCst);
        // Make sure every chunk of output is delivered before announcing the end.
        for reader in readers {
            let _ = reader.join();
        }
        let outcome = match status {
            _ if cancelled_flag.load(Ordering::SeqCst) => RunOutcome::Cancelled,
            Ok(status) if status.success() => RunOutcome::Succeeded,
            Ok(status) => RunOutcome::Failed {
                code: status.code(),
            },
            Err(error) => RunOutcome::NotStarted(format!("lost track of the process: {error}")),
        };
        let _ = tx.send(Event::RunFinished(outcome));
    });

    Ok(RunningJob {
        pgid,
        finished,
        cancelled,
    })
}

fn spawn_with_retry(dvc: &Path, root: &Path, args: &[String]) -> io::Result<std::process::Child> {
    let mut command = Command::new(dvc);
    command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A piped Python stdout is block-buffered; without this, output appears only at exit.
        .env("PYTHONUNBUFFERED", "1")
        .env("NO_COLOR", "1")
        // Lead a new process group, so one signal can reach dvc *and* the stage command it starts.
        .process_group(0);

    let mut attempt = 1;
    loop {
        match command.spawn() {
            // A file that was written a moment ago can still look "busy" to exec if another
            // thread forked while it was open. It clears up almost immediately.
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && attempt < SPAWN_RETRIES => {
                attempt += 1;
                thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

fn spawn_pump(reader: impl Read + Send + 'static, tx: Sender<Event>) -> JoinHandle<()> {
    thread::spawn(move || pump(reader, &tx))
}

/// Forward everything `reader` produces as `Event::RunOutput`, until it is closed.
fn pump(mut reader: impl Read, tx: &Sender<Event>) {
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buffer[..n]);
                let text = drain_utf8(&mut pending);
                if !text.is_empty() && tx.send(Event::RunOutput(text)).is_err() {
                    return; // nobody is listening any more
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let _ = tx.send(Event::RunOutput(
            String::from_utf8_lossy(&pending).into_owned(),
        ));
    }
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
fn signal_group(pgid: u32, signal: &str) {
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
        signal_group(self.pgid, "TERM");

        let (pgid, finished) = (self.pgid, Arc::clone(&self.finished));
        thread::spawn(move || {
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline {
                if finished.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(POLL_INTERVAL);
            }
            if !finished.load(Ordering::SeqCst) {
                signal_group(pgid, "KILL");
            }
        });
    }

    /// Cancel and wait (up to `timeout`) until the process has really exited. Used when quitting.
    pub fn terminate_blocking(&self, grace: Duration, timeout: Duration) -> bool {
        self.cancel_with_grace(grace);
        let deadline = Instant::now() + timeout;
        while !self.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        self.is_finished()
    }
}

/// Safety net: if a job is dropped while its process still runs (for example during a panic),
/// kill it rather than leave an orphan behind.
impl Drop for RunningJob {
    fn drop(&mut self) {
        if !self.is_finished() {
            signal_group(self.pgid, "KILL");
        }
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
    fn not_found_lists_every_place_searched() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope/dvc");
        let path_var = env::join_paths([dir.path().join("a"), dir.path().join("b")]).unwrap();
        let error = find_dvc(dir.path(), Some(missing.as_os_str()), Some(&path_var)).unwrap_err();
        assert_eq!(error.searched.len(), 3, "{error}");
        let message = error.to_string();
        assert!(message.contains("DVC_TUI_DVC"), "{message}");
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
    fn dropping_a_running_job_kills_it() {
        let dir = tempfile::tempdir().unwrap();
        let dvc = fake_dvc(dir.path(), "sleep 60 &\necho $! > grandchild.pid\nwait");
        let (tx, _rx) = mpsc::channel();
        let job = start(&dvc, dir.path(), &[], tx).unwrap();
        let grandchild: u32 = wait_for_file(&dir.path().join("grandchild.pid"))
            .trim()
            .parse()
            .unwrap();
        drop(job);
        assert!(wait_until_gone(grandchild));
    }
}
