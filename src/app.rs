//! Application state and the one place that changes it.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    graph::Graph,
    log::LogBuffer,
    project::{Project, Stage},
    runner::{self, DvcNotFound, KILL_GRACE, RunOutcome, RunningJob},
    stale::{self, StageStatus},
};

/// How many lines PageUp/PageDown move the log.
const LOG_SCROLL_STEP: usize = 10;
/// Extra time, beyond the kill grace period, that quitting waits for a run to exit.
const QUIT_WAIT: Duration = Duration::from_secs(2);

/// Everything that can happen to the app arrives as an `Event`.
/// Later groups add variants here (run output, experiment results, ...).
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    /// Result of a background stale check, indexed like `project.stages`.
    Statuses(Vec<StageStatus>),
    /// A chunk of the running command's combined stdout/stderr.
    RunOutput(String),
    /// The run ended (or never started). Always the last event of a run.
    RunFinished(RunOutcome),
}

/// Something `update` wants done but must not do itself, because it touches the outside world.
/// `update` stays a pure function of state; [`App::dispatch`] carries the effects out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    StartRun {
        stage: String,
        with_upstream: bool,
    },
    CancelRun,
    /// Re-read `dvc.lock` and recompute every stage's status.
    RefreshStatuses,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPhase {
    Running,
    /// Cancel was requested; waiting for the process to exit.
    Cancelling,
    Finished(RunOutcome),
}

/// The most recent run (kept on screen after it ends, until the next one replaces it).
#[derive(Debug)]
pub struct Run {
    pub stage: String,
    pub with_upstream: bool,
    pub log: LogBuffer,
    pub phase: RunPhase,
    /// Lines scrolled back from the bottom; 0 follows new output.
    pub scroll: usize,
    job: Option<RunningJob>,
}

impl Run {
    pub fn new(stage: String, with_upstream: bool) -> Self {
        Self {
            stage,
            with_upstream,
            log: LogBuffer::new(),
            phase: RunPhase::Running,
            scroll: 0,
            job: None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, RunPhase::Running | RunPhase::Cancelling)
    }

    pub fn command_line(&self) -> String {
        format!(
            "dvc {}",
            runner::repro_args(&self.stage, self.with_upstream).join(" ")
        )
    }
}

#[derive(Debug)]
pub struct App {
    pub project: Project,
    pub graph: Graph,
    /// Position in the displayed (topological) order, *not* an index into `project.stages`.
    pub selected: usize,
    /// `None` while the first stale check is still running.
    pub statuses: Option<Vec<StageStatus>>,
    pub run: Option<Run>,
    /// A one-line message for the footer; cleared by the next key press.
    pub notice: Option<String>,
    /// How to find `dvc`. A plain function pointer so tests can substitute their own.
    pub dvc_lookup: fn(&Path) -> Result<PathBuf, DvcNotFound>,
    pub should_quit: bool,
}

impl App {
    pub fn new(project: Project, graph: Graph) -> Self {
        Self {
            project,
            graph,
            selected: 0,
            statuses: None,
            run: None,
            notice: None,
            dvc_lookup: runner::find_dvc_in_env,
            should_quit: false,
        }
    }

    pub fn stage_count(&self) -> usize {
        self.graph.order.len()
    }

    /// The stage shown at `position` in the list. The returned reference borrows from `self`.
    pub fn stage_at(&self, position: usize) -> Option<&Stage> {
        let index = *self.graph.order.get(position)?;
        self.project.stages.get(index)
    }

    /// The stale-check result for the stage shown at `position`, once it has arrived.
    pub fn status_at(&self, position: usize) -> Option<&StageStatus> {
        let index = *self.graph.order.get(position)?;
        self.statuses.as_ref()?.get(index)
    }

    /// Check staleness on a background thread (hashing files can be slow) and report back
    /// as `Event::Statuses`. The UI keeps running meanwhile.
    pub fn spawn_status_check(&self, tx: &Sender<Event>) {
        // The thread needs its own copies: it may outlive this call.
        let project = self.project.clone();
        let graph = self.graph.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let statuses = stale::compute(&project, &graph);
            // Err means the app already quit; there is nobody left to tell.
            let _ = tx.send(Event::Statuses(statuses));
        });
    }

    pub fn selected_stage(&self) -> Option<&Stage> {
        self.stage_at(self.selected)
    }

    /// Names of the stages the selected stage depends on (empty if none or nothing selected).
    pub fn selected_upstream(&self) -> Vec<&str> {
        let Some(&index) = self.graph.order.get(self.selected) else {
            return Vec::new();
        };
        self.graph.upstream[index]
            .iter()
            .map(|&up| self.project.stages[up].name.as_str())
            .collect()
    }

    /// The only function that mutates the app (`&mut self`). It returns the side effects it wants.
    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) => {
                self.notice = None;
                self.on_key(key)
            }
            Event::Statuses(statuses) => {
                self.statuses = Some(statuses);
                Vec::new()
            }
            Event::RunOutput(text) => {
                if let Some(run) = &mut self.run {
                    run.log.push_str(&text);
                }
                Vec::new()
            }
            Event::RunFinished(outcome) => self.on_run_finished(outcome),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // Raw mode turns off the terminal's own Ctrl+C handling, so we do it ourselves.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection_up(),
            KeyCode::Char('r') => return self.request_run(false),
            KeyCode::Char('R') => return self.request_run(true),
            KeyCode::Char('x') => return self.request_cancel(),
            KeyCode::PageUp => self.scroll_log_back(),
            KeyCode::PageDown => self.scroll_log_forward(),
            _ => {}
        }
        Vec::new()
    }

    pub fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(Run::is_active)
    }

    fn request_run(&mut self, with_upstream: bool) -> Vec<Effect> {
        if self.is_running() {
            self.notice = Some("a run is already in progress (x cancels it)".to_owned());
            return Vec::new();
        }
        let Some(stage) = self.selected_stage() else {
            return Vec::new();
        };
        if let Some(reason) = &stage.unsupported {
            self.notice = Some(format!("cannot run '{}': {reason}", stage.name));
            return Vec::new();
        }
        let name = stage.name.clone();
        self.run = Some(Run::new(name.clone(), with_upstream));
        vec![Effect::StartRun {
            stage: name,
            with_upstream,
        }]
    }

    fn request_cancel(&mut self) -> Vec<Effect> {
        match self.run.as_mut() {
            Some(run) if run.phase == RunPhase::Running => {
                run.phase = RunPhase::Cancelling;
                vec![Effect::CancelRun]
            }
            Some(run) if run.phase == RunPhase::Cancelling => Vec::new(),
            _ => {
                self.notice = Some("nothing is running".to_owned());
                Vec::new()
            }
        }
    }

    fn on_run_finished(&mut self, outcome: RunOutcome) -> Vec<Effect> {
        let Some(run) = &mut self.run else {
            return Vec::new();
        };
        if let RunOutcome::NotStarted(message) = &outcome {
            run.log.push_str(message);
        }
        run.log.finish();
        run.job = None; // the process is gone; dropping the handle is now harmless
        let refresh = !matches!(outcome, RunOutcome::NotStarted(_));
        run.phase = RunPhase::Finished(outcome);
        if refresh {
            // Whatever ran may have rewritten dvc.lock, so earlier results are out of date.
            self.statuses = None;
            vec![Effect::RefreshStatuses]
        } else {
            Vec::new()
        }
    }

    fn scroll_log_back(&mut self) {
        if let Some(run) = &mut self.run {
            let furthest = run.log.len().saturating_sub(1);
            run.scroll = (run.scroll + LOG_SCROLL_STEP).min(furthest);
        }
    }

    fn scroll_log_forward(&mut self) {
        if let Some(run) = &mut self.run {
            run.scroll = run.scroll.saturating_sub(LOG_SCROLL_STEP);
        }
    }

    /// Handle an event and then carry out whatever side effects it asked for.
    pub fn dispatch(&mut self, event: Event, tx: &Sender<Event>) {
        for effect in self.update(event) {
            self.execute(effect, tx);
        }
    }

    fn execute(&mut self, effect: Effect, tx: &Sender<Event>) {
        match effect {
            Effect::StartRun {
                stage,
                with_upstream,
            } => self.start_run(&stage, with_upstream, tx),
            Effect::CancelRun => {
                if let Some(job) = self.run.as_ref().and_then(|run| run.job.as_ref()) {
                    job.cancel();
                }
            }
            Effect::RefreshStatuses => {
                if let Err(error) = self.project.reload_state() {
                    self.notice = Some(format!("could not reload project state: {error:#}"));
                }
                self.spawn_status_check(tx);
            }
        }
    }

    fn start_run(&mut self, stage: &str, with_upstream: bool, tx: &Sender<Event>) {
        let root = self.project.root.clone();
        let args = runner::repro_args(stage, with_upstream);
        let started = (self.dvc_lookup)(&root)
            .map_err(|error| error.to_string())
            .and_then(|dvc| {
                runner::start(&dvc, &root, &args, tx.clone())
                    .map_err(|error| format!("cannot start {}: {error}", dvc.display()))
            });
        match started {
            Ok(job) => {
                if let Some(run) = &mut self.run {
                    run.job = Some(job);
                }
            }
            // Report a failure to start the same way as any other end of a run.
            Err(message) => self.dispatch(Event::RunFinished(RunOutcome::NotStarted(message)), tx),
        }
    }

    /// Stop any running process and wait for it to exit. Call before the program ends.
    pub fn shutdown(&mut self) {
        if let Some(job) = self.run.as_ref().and_then(|run| run.job.as_ref()) {
            job.terminate_blocking(KILL_GRACE, KILL_GRACE + QUIT_WAIT);
        }
    }

    fn move_selection_down(&mut self) {
        if self.selected + 1 < self.stage_count() {
            self.selected += 1;
        }
    }

    fn move_selection_up(&mut self) {
        // `saturating_sub` stops at 0 instead of underflowing (usize can't be negative).
        self.selected = self.selected.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{app_with_files, demo_app};

    fn app() -> App {
        demo_app()
    }

    fn press(app: &mut App, code: KeyCode) {
        app.update(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn selected_upstream_lists_dependency_names() {
        let mut app = app();
        assert!(app.selected_upstream().is_empty()); // prepare
        app.selected = 2;
        assert_eq!(app.selected_upstream(), ["features"]); // train
    }

    #[test]
    fn statuses_start_unknown_and_arrive_as_an_event() {
        let mut app = app();
        assert!(app.status_at(0).is_none());
        app.update(Event::Statuses(stale::compute(&app.project, &app.graph)));
        assert_eq!(app.status_at(0).unwrap().status(), stale::Status::UpToDate);
        assert!(app.status_at(99).is_none());
    }

    #[test]
    fn background_check_reports_through_the_channel() {
        let mut app = app();
        let (tx, rx) = std::sync::mpsc::channel();
        app.spawn_status_check(&tx);
        let event = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        app.update(event);
        let statuses = app.statuses.as_ref().expect("statuses delivered");
        assert_eq!(statuses.len(), 3);
        assert!(
            statuses
                .iter()
                .all(|s| s.status() == stale::Status::UpToDate)
        );
    }

    #[test]
    fn q_quits() {
        let mut app = app();
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app();
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut app = app();
        press(&mut app, KeyCode::Char('c'));
        assert!(!app.should_quit);
    }

    #[test]
    fn j_and_down_move_down() {
        let mut app = app();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 1);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn selection_stops_at_last_item() {
        let mut app = app();
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn k_and_up_move_up_and_stop_at_zero() {
        let mut app = app();
        app.selected = 2;
        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Up);
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn movement_on_empty_list_is_safe() {
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", "stages: {}\n")]);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected, 0);
        assert!(app.selected_stage().is_none());
        assert!(app.selected_upstream().is_empty());
    }

    #[test]
    fn selection_follows_dependency_order_not_declaration_order() {
        let yaml = "stages:\n  train:\n    cmd: t\n    deps: [f.csv]\n  features:\n    cmd: f\n    outs: [f.csv]\n";
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", yaml)]);
        assert_eq!(app.selected_stage().unwrap().name, "features");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_stage().unwrap().name, "train");
        assert_eq!(app.selected_upstream(), ["features"]);
    }

    #[test]
    fn unknown_key_changes_nothing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('z'));
        assert_eq!(app.selected, 0);
        assert!(!app.should_quit);
    }
}

#[cfg(test)]
mod run_tests {
    use std::{fs, os::unix::fs::PermissionsExt, sync::mpsc, time::Instant};

    use tempfile::TempDir;

    use super::*;
    use crate::test_support::app_with_files;

    const TWO_STAGES: &str =
        "stages:\n  a:\n    cmd: x\n    outs: [a.txt]\n  b:\n    cmd: y\n    deps: [a.txt]\n";
    const WAIT: Duration = Duration::from_secs(10);

    /// An app wired to a fake `dvc` script, with the channel the real main loop would own.
    struct Harness {
        dir: TempDir,
        app: App,
        tx: Sender<Event>,
        rx: mpsc::Receiver<Event>,
    }

    fn harness(script: &str) -> Harness {
        let (dir, mut app) = app_with_files(&[("dvc.yaml", TWO_STAGES)]);
        let path = dir.path().join("fake-dvc");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        app.dvc_lookup = |root| Ok(root.join("fake-dvc"));
        let (tx, rx) = mpsc::channel();
        Harness { dir, app, tx, rx }
    }

    impl Harness {
        fn press(&mut self, code: KeyCode) {
            let key = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
            self.app.dispatch(key, &self.tx);
        }

        /// Deliver background events to the app until `done` holds.
        fn pump_until(&mut self, done: impl Fn(&App) -> bool) {
            let deadline = Instant::now() + WAIT;
            while !done(&self.app) {
                let left = deadline.saturating_duration_since(Instant::now());
                let event = self
                    .rx
                    .recv_timeout(left)
                    .expect("timed out waiting for an event");
                self.app.dispatch(event, &self.tx);
            }
        }

        fn pump_until_finished(&mut self) {
            self.pump_until(|app| app.run.as_ref().is_some_and(|r| !r.is_active()));
        }

        fn lines(&self) -> Vec<String> {
            self.app
                .run
                .as_ref()
                .unwrap()
                .log
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn phase(&self) -> RunPhase {
            self.app.run.as_ref().unwrap().phase.clone()
        }

        fn cancel_and_wait(&mut self) {
            self.press(KeyCode::Char('x'));
            self.pump_until_finished();
        }
    }

    #[test]
    fn r_runs_only_the_selected_stage_and_shows_its_output() {
        let mut h = harness("echo \"args: $*\"");
        h.press(KeyCode::Char('r'));
        assert!(h.app.is_running());
        h.pump_until_finished();
        assert_eq!(h.lines(), ["args: repro --single-item a"]);
        assert_eq!(h.phase(), RunPhase::Finished(RunOutcome::Succeeded));
    }

    #[test]
    fn capital_r_runs_with_upstream_stages() {
        let mut h = harness("echo \"args: $*\"");
        h.press(KeyCode::Char('j')); // stage b
        h.press(KeyCode::Char('R'));
        h.pump_until_finished();
        assert_eq!(h.lines(), ["args: repro b"]);
    }

    #[test]
    fn a_finished_run_invalidates_statuses_and_recomputes_them() {
        let mut h = harness("true");
        h.app.statuses = Some(Vec::new());
        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        assert!(
            h.app.statuses.is_none(),
            "old results must not linger after a run"
        );
        h.pump_until(|app| app.statuses.is_some());
        assert_eq!(h.app.statuses.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn failure_shows_the_exit_code_and_keeps_the_log() {
        let mut h = harness("echo went wrong; exit 2");
        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        assert_eq!(
            h.phase(),
            RunPhase::Finished(RunOutcome::Failed { code: Some(2) })
        );
        assert_eq!(h.lines(), ["went wrong"]);
        h.pump_until(|app| app.statuses.is_some()); // a failed run may still have changed dvc.lock
    }

    #[test]
    fn second_run_is_refused_while_one_is_active() {
        let mut h = harness("sleep 30");
        h.press(KeyCode::Char('r'));
        h.press(KeyCode::Char('j'));
        h.press(KeyCode::Char('r'));
        assert!(
            h.app
                .notice
                .as_deref()
                .unwrap()
                .contains("already in progress")
        );
        assert_eq!(
            h.app.run.as_ref().unwrap().stage,
            "a",
            "the first run is untouched"
        );
        h.cancel_and_wait();
    }

    #[test]
    fn selection_and_log_scrolling_work_during_a_run() {
        let mut h = harness("echo first; sleep 30");
        h.press(KeyCode::Char('r'));
        h.pump_until(|app| !app.run.as_ref().unwrap().log.is_empty());
        assert!(h.app.is_running());
        h.press(KeyCode::Char('j'));
        assert_eq!(h.app.selected, 1);
        h.press(KeyCode::PageUp);
        assert_eq!(
            h.app.run.as_ref().unwrap().scroll,
            0,
            "only one line, nothing to scroll back to"
        );
        h.cancel_and_wait();
    }

    #[test]
    fn cancel_goes_through_cancelling_to_cancelled_and_allows_a_new_run() {
        let mut h = harness("sleep 30");
        h.press(KeyCode::Char('r'));
        h.press(KeyCode::Char('x'));
        assert_eq!(h.phase(), RunPhase::Cancelling);
        h.pump_until_finished();
        assert_eq!(h.phase(), RunPhase::Finished(RunOutcome::Cancelled));
        assert!(!h.app.is_running());

        h.press(KeyCode::Char('r'));
        assert!(h.app.is_running(), "a new run can start after a cancel");
        h.cancel_and_wait();
    }

    #[test]
    fn cancelling_with_nothing_running_says_so() {
        let mut h = harness("true");
        h.press(KeyCode::Char('x'));
        assert_eq!(h.app.notice.as_deref(), Some("nothing is running"));
        assert!(h.app.run.is_none());
    }

    #[test]
    fn notices_clear_on_the_next_key() {
        let mut h = harness("true");
        h.press(KeyCode::Char('x'));
        assert!(h.app.notice.is_some());
        h.press(KeyCode::Char('j'));
        assert!(h.app.notice.is_none());
    }

    #[test]
    fn missing_dvc_prevents_the_run_and_says_where_it_looked() {
        let mut h = harness("true");
        h.app.dvc_lookup = |_| {
            Err(DvcNotFound {
                searched: vec!["place one".into(), "place two".into()],
            })
        };
        h.press(KeyCode::Char('r'));
        assert!(!h.app.is_running());
        let RunPhase::Finished(RunOutcome::NotStarted(message)) = h.phase() else {
            panic!("expected NotStarted, got {:?}", h.phase());
        };
        assert!(
            message.contains("place one") && message.contains("place two"),
            "{message}"
        );
        assert!(
            h.lines().join("\n").contains("place one"),
            "the log tells the user too"
        );
        assert!(
            h.rx.try_recv().is_err(),
            "nothing started, so nothing to refresh"
        );
    }

    #[test]
    fn a_program_that_cannot_be_executed_is_reported_not_started() {
        let mut h = harness("true");
        h.app.dvc_lookup = |root| Ok(root.join("does-not-exist"));
        h.press(KeyCode::Char('r'));
        let RunPhase::Finished(RunOutcome::NotStarted(message)) = h.phase() else {
            panic!("expected NotStarted");
        };
        assert!(message.contains("cannot start"), "{message}");
    }

    #[test]
    fn stages_we_cannot_expand_are_not_run() {
        let yaml = "stages:\n  each:\n    foreach: [a]\n    do:\n      cmd: echo\n";
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", yaml)]);
        let effects = app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        assert!(effects.is_empty());
        assert!(app.notice.as_deref().unwrap().contains("foreach"));
        assert!(app.run.is_none());
    }

    #[test]
    fn update_only_describes_side_effects_it_does_not_perform() {
        let mut app = crate::test_support::demo_app();
        let r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(
            app.update(Event::Key(r)),
            [Effect::StartRun {
                stage: "prepare".into(),
                with_upstream: false
            }]
        );
        let x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.update(Event::Key(x)), [Effect::CancelRun]);
    }

    #[test]
    fn log_scrolling_is_clamped_to_the_available_lines() {
        let mut app = crate::test_support::demo_app();
        let mut run = Run::new("prepare".into(), false);
        run.log.push_str(&"line\n".repeat(25));
        app.run = Some(run);
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        for _ in 0..10 {
            app.update(key(KeyCode::PageUp));
        }
        assert_eq!(app.run.as_ref().unwrap().scroll, 24);
        app.update(key(KeyCode::PageDown));
        assert_eq!(app.run.as_ref().unwrap().scroll, 14);
        for _ in 0..10 {
            app.update(key(KeyCode::PageDown));
        }
        assert_eq!(app.run.as_ref().unwrap().scroll, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn shutdown_stops_a_running_job_and_everything_it_started() {
        let mut h = harness("sleep 60 &\necho $! > grandchild.pid\nwait");
        h.press(KeyCode::Char('r'));
        let pid_file = h.dir.path().join("grandchild.pid");
        let deadline = Instant::now() + WAIT;
        let grandchild: u32 = loop {
            if let Ok(text) = fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "grandchild never started");
            thread::sleep(Duration::from_millis(10));
        };

        h.app.shutdown();

        let is_gone = || match fs::read_to_string(format!("/proc/{grandchild}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|r| r.trim_start().starts_with('Z')),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !is_gone() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(is_gone(), "process {grandchild} outlived shutdown");
    }
}
