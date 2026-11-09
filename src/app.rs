//! Application state and the one place that changes it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    dag::{self, Layout, Move},
    dvc_yaml,
    experiments::{self, RowKind, Sort, Table},
    external,
    files::{self, FileEntry, Preview},
    form::{ExperimentForm, FormAction},
    graph::{self, Graph},
    history::{self, History, StoredOutcome, StoredRun},
    log::LogBuffer,
    messages,
    params_filter::{ParamPicker, PickerAction},
    plot_form::{PlotForm, PlotFormAction},
    plots::{self, Plot},
    project::{self, DEFAULT_PARAMS_FILE, Project, Stage},
    prompt::{Prompt, PromptAction, PromptKind},
    remotes::{self, Remote, RemoteChoice, RemotePicker, Transfer},
    runner::{self, Attached, DvcNotFound, KILL_GRACE, RunOutcome, RunRecord, RunningJob},
    stage_form::{self, Pick, StageForm, StageFormAction},
    stale::{self, StageStatus},
};

/// At most this many files are offered to `fzf`.
const MAX_PICK_FILES: usize = 5000;
/// How many lines PageUp/PageDown move a preview.
const PREVIEW_PAGE: usize = 15;
/// How many lines PageUp/PageDown move the log.
const LOG_SCROLL_STEP: usize = 10;
/// Extra time, beyond the kill grace period, that quitting waits for a run to exit.
const QUIT_WAIT: Duration = Duration::from_secs(2);

/// Everything that can happen to the app arrives as an `Event`.
/// Later groups add variants here (run output, experiment results, ...).
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    /// The remotes found by a background `dvc remote list`, for a pull or a push.
    Remotes(Transfer, Result<Vec<Remote>, String>),
    /// Result of a background stale check, indexed like `project.stages`.
    Statuses(Vec<StageStatus>),
    /// A chunk of the running command's combined stdout/stderr.
    RunOutput(String),
    /// The run ended (or never started). Always the last event of a run.
    RunFinished(RunOutcome),
    /// The result of a background `dvc exp show --json`.
    Experiments(Result<Table, String>),
    /// A file's content, read for the preview popup.
    PreviewLoaded(Preview),
}

/// How often the lock file is re-read to notice `dvc` runs started elsewhere.
const EXTERNAL_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// A yes/no question shown before something destructive. Modal, like the preview and the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    /// Names the run in the UI once confirmed.
    pub label: String,
    pub args: Vec<String>,
    /// When not empty, confirming stops these `dvc` processes (started outside dvcat) instead of
    /// running `args`.
    pub kill: Vec<u32>,
    /// Options the user can switch on before confirming (each adds `args` to the command).
    pub toggles: Vec<Toggle>,
}

/// An option of a [`Confirm`], switched by pressing its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toggle {
    pub key: char,
    pub text: String,
    pub on: bool,
    /// Appended to the command while the option is on.
    pub args: Vec<String>,
}

impl Confirm {
    /// The label and arguments of the run: the base ones plus those of the options that are on.
    /// With options, the label is the command line itself, so the header shows what runs.
    pub fn command(&self) -> (String, Vec<String>) {
        let mut args = self.args.clone();
        for toggle in self.toggles.iter().filter(|t| t.on) {
            args.extend(toggle.args.iter().cloned());
        }
        let label = if self.toggles.is_empty() {
            self.label.clone()
        } else {
            format!("dvc {}", args.join(" "))
        };
        (label, args)
    }
}

/// The question asked when a run could not start because other `dvc` processes hold the lock.
fn blocked_confirm(blocking: &[external::ExternalRun]) -> Confirm {
    let mut lines = vec![
        "Another dvc process, not started by dvcat, holds the lock this run needs:".to_owned(),
    ];
    lines.extend(
        blocking
            .iter()
            .map(|run| format!("  PID {}: {}", run.pid, run.short_command())),
    );
    lines.push("Stop it? Its unfinished work is lost; DVC cleans up its lock.".to_owned());
    Confirm {
        title: "Blocked by a running dvc".to_owned(),
        lines,
        label: "stop external dvc".to_owned(),
        args: Vec::new(),
        kill: blocking.iter().map(|run| run.pid).collect(),
        toggles: Vec::new(),
    }
}

/// Which pane of the pipeline view receives the movement keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Stages,
    Files,
}

/// What `a` / `d` ask about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Apply,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Pipeline,
    Experiments,
    /// The stage graph, drawn as boxes and lines.
    Dag,
    /// The plots defined in `dvc.yaml`, drawn from their data files.
    Plots,
}

/// Which form field a pick from `fzf` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickTarget {
    PlotFile,
    Stage(Pick),
}

/// A list of paths (or param keys) for `fzf` to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRequest {
    pub items: Vec<String>,
    /// Several items may be chosen (Tab in `fzf`).
    pub multi: bool,
    pub prompt: String,
    pub target: PickTarget,
}

/// Everything the plots view needs to remember.
#[derive(Debug, Default)]
pub struct PlotsView {
    /// `None` until the view is first opened; read again by F5.
    pub data: Option<Result<Vec<Plot>, String>>,
    pub selected: usize,
    /// Show the selected plot's definition from `dvc.yaml` under the chart (`d`).
    pub definition: bool,
}

/// Everything the experiments view needs to remember.
#[derive(Debug, Default)]
pub struct ExperimentsView {
    /// `None` until the first fetch finishes. The last result stays visible during a refresh.
    pub data: Option<Result<Table, String>>,
    pub loading: bool,
    /// Cursor: a row in display order and a column in `Table::columns()`.
    pub row: usize,
    pub column: usize,
    pub sort: Sort,
    /// Show metrics of experiments as differences from their baseline (`m`).
    pub deltas: bool,
    /// Show the chart under the table (`p`).
    pub plot: bool,
    /// Which plottable metric the chart shows (`[` / `]`); taken modulo the number of metrics.
    pub plot_metric: usize,
    /// Experiments across the top and params/metrics down the side (`t`).
    pub transpose: bool,
    /// Param columns that are not shown (`v` / `H`).
    pub hidden: BTreeSet<String>,
}

impl ExperimentsView {
    pub fn table(&self) -> Option<&Table> {
        self.data.as_ref()?.as_ref().ok()
    }

    /// The columns on screen: those of `table` minus the hidden params.
    pub fn columns(&self, table: &Table) -> Vec<experiments::Column> {
        table.columns_hiding(&self.hidden)
    }
}

/// Something `update` wants done but must not do itself, because it touches the outside world.
/// `update` stays a pure function of state; [`App::dispatch`] carries the effects out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    StartRun {
        stage: String,
        with_upstream: bool,
    },
    /// Start `dvc exp run`; the arguments are already stored in `App::run`.
    StartExperiment,
    CancelRun,
    /// Re-read `dvc.lock` and recompute every stage's status.
    RefreshStatuses,
    /// Run `dvc exp show --json` in the background.
    FetchExperiments,
    /// List the remotes in the background, then offer them for a pull or a push.
    LoadRemotes(Transfer),
    /// Read a project file (path relative to the project root) for the preview popup.
    LoadPreview(String),
    /// Hand a project file to `$EDITOR`. The terminal belongs to `main`, so we only leave a request.
    OpenEditor(String),
    /// Stop `dvc` processes that were started outside dvcat (by process id).
    StopExternal(Vec<u32>),
    /// Give the experiment commit `rev` a new message, then reload the table.
    SaveMessage {
        rev: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPhase {
    Running,
    /// Cancel was requested; waiting for the process to exit.
    Cancelling,
    Finished(RunOutcome),
}

/// What a run is for; decides what happens when it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKind {
    Stage,
    /// `exp run`, `exp apply` or `exp remove`: the experiments table is reloaded afterwards.
    Experiment,
    /// `exp branch`: nothing changes in the workspace; a success notice names the branch.
    Branch {
        name: String,
    },
    /// `dvc pull` / `dvc push`: only the stage statuses are re-checked afterwards.
    Remote,
}

/// The most recent run (kept on screen after it ends, until the next one replaces it).
#[derive(Debug)]
pub struct Run {
    /// What ran: a stage name, or `exp <name>` for an experiment.
    pub stage: String,
    /// The `dvc` arguments, shown in the log title and used to start the process.
    pub args: Vec<String>,
    pub kind: RunKind,
    pub with_upstream: bool,
    pub log: LogBuffer,
    pub phase: RunPhase,
    /// Lines scrolled back from the bottom; 0 follows new output.
    pub scroll: usize,
    /// When the run was requested.
    started: Instant,
    /// How long it took; `None` while it is still going.
    elapsed: Option<Duration>,
    /// A duration known from elsewhere (a run that ended while the interface was closed), used
    /// instead of the time since `started`.
    fixed_duration: Option<Duration>,
    job: Option<RunningJob>,
}

/// What the previous run of a stage in this session ended with, and how long it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub outcome: RunOutcome,
    pub duration: Duration,
}

impl Run {
    /// Did DVC skip the requested stage because nothing changed? (It says so in the log.)
    /// Upstream stages that were skipped do not matter: only the stage that was asked for.
    fn skipped_target(&self) -> bool {
        let message = format!("Stage '{}' didn't change, skipping", self.stage);
        self.log.lines().any(|line| line.contains(&message))
    }

    /// Time since the run started, or its final duration once it has ended.
    pub fn duration(&self) -> Duration {
        self.elapsed.unwrap_or_else(|| self.started.elapsed())
    }

    pub fn new(stage: String, with_upstream: bool) -> Self {
        let args = runner::repro_args(&stage, with_upstream);
        Self::with_args(stage, args, with_upstream, RunKind::Stage)
    }

    /// An experiment run; `label` is shown in the header.
    pub fn experiment(label: String, args: Vec<String>) -> Self {
        Self::with_args(label, args, false, RunKind::Experiment)
    }

    /// `dvc pull` or `dvc push`.
    pub fn remote(label: String, args: Vec<String>) -> Self {
        Self::with_args(label, args, false, RunKind::Remote)
    }

    pub fn branch(label: String, args: Vec<String>, name: String) -> Self {
        Self::with_args(label, args, false, RunKind::Branch { name })
    }

    fn with_args(stage: String, args: Vec<String>, with_upstream: bool, kind: RunKind) -> Self {
        Self {
            stage,
            args,
            kind,
            with_upstream,
            log: LogBuffer::new(),
            phase: RunPhase::Running,
            scroll: 0,
            started: Instant::now(),
            elapsed: None,
            fixed_duration: None,
            job: None,
        }
    }

    /// The part of this run that [`runner::attach`] needs to find it again.
    fn record(&self) -> RunRecord {
        let (kind, branch) = match &self.kind {
            RunKind::Stage => ("stage", None),
            RunKind::Experiment => ("experiment", None),
            RunKind::Branch { name } => ("branch", Some(name.clone())),
            RunKind::Remote => ("remote", None),
        };
        let started_ms = SystemTime::now()
            .checked_sub(self.started.elapsed())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        RunRecord {
            pgid: 0,
            args: self.args.clone(),
            stage: self.stage.clone(),
            kind: kind.to_owned(),
            branch,
            with_upstream: self.with_upstream,
            started_ms,
        }
    }

    /// A run from an earlier session, as found by [`runner::attach`].
    fn restored(record: &RunRecord) -> Self {
        let kind = match (record.kind.as_str(), &record.branch) {
            ("experiment", _) => RunKind::Experiment,
            ("remote", _) => RunKind::Remote,
            ("branch", Some(name)) => RunKind::Branch { name: name.clone() },
            _ => RunKind::Stage,
        };
        let mut run = Self::with_args(
            record.stage.clone(),
            record.args.clone(),
            record.with_upstream,
            kind,
        );
        // Count the time since it really started, not since this session did.
        let since_start = Duration::from_millis(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
                .saturating_sub(record.started_ms),
        );
        run.started = Instant::now()
            .checked_sub(since_start)
            .unwrap_or_else(Instant::now);
        run
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, RunPhase::Running | RunPhase::Cancelling)
    }

    pub fn command_line(&self) -> String {
        format!("dvc {}", self.args.join(" "))
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
    pub view: View,
    pub experiments: ExperimentsView,
    pub plots: PlotsView,
    pub focus: Focus,
    /// Where the graph view draws each stage; computed once, because the stages do not change.
    pub dag: Layout,
    /// The open "new experiment" form, if any. Like the preview, it is modal.
    pub form: Option<ExperimentForm>,
    /// The open "new plot" form, if any; modal as well.
    pub plot_form: Option<PlotForm>,
    /// The open "new stage" form, if any; modal as well.
    pub stage_form: Option<StageForm>,
    pub confirm: Option<Confirm>,
    /// `dvc` processes started outside dvcat that hold this project's lock (refreshed by [`App::tick`]).
    pub external: Vec<external::ExternalRun>,
    external_checked: Option<Instant>,
    pub prompt: Option<Prompt>,
    /// The open list for hiding param columns; modal like the preview.
    pub param_picker: Option<ParamPicker>,
    pub remote_picker: Option<RemotePicker>,
    /// The latest finished run of each stage started from this session (DVC keeps no durations).
    pub last_runs: BTreeMap<String, LastRun>,
    /// Seconds each experiment took, by name, for the experiments table.
    pub experiment_times: BTreeMap<String, f64>,
    /// Whether run times are read from and written to the project's history file (`main` turns it on).
    persist_history: bool,
    /// Cursor in the files pane, an index into [`App::selected_files`].
    pub file_cursor: usize,
    /// The open preview popup, if any. While it is open it receives all keys.
    pub preview: Option<Preview>,
    /// Set when the user asked for the editor; `main` takes it, suspends the UI and runs the editor.
    pub editor_request: Option<PathBuf>,
    /// Set when the new-plot form wants a file chosen; `main` takes it, suspends the UI and runs
    /// `fzf` over these paths, then reports the choice to [`App::file_picked`].
    pub pick_request: Option<PickRequest>,
    /// A one-line message for the footer; cleared by the next key press.
    pub notice: Option<String>,
    /// How to find `dvc`. A plain function pointer so tests can substitute their own.
    pub dvc_lookup: fn(&Path) -> Result<PathBuf, DvcNotFound>,
    pub should_quit: bool,
}

impl App {
    pub fn new(project: Project, graph: Graph) -> Self {
        let names: Vec<&str> = project.stages.iter().map(|s| s.name.as_str()).collect();
        let dag = dag::layout(&names, &graph);
        Self {
            project,
            graph,
            dag,
            selected: 0,
            statuses: None,
            run: None,
            view: View::Pipeline,
            experiments: ExperimentsView::default(),
            plots: PlotsView::default(),
            focus: Focus::Stages,
            form: None,
            plot_form: None,
            stage_form: None,
            confirm: None,
            external: Vec::new(),
            external_checked: None,
            prompt: None,
            param_picker: None,
            remote_picker: None,
            last_runs: BTreeMap::new(),
            experiment_times: BTreeMap::new(),
            persist_history: false,
            file_cursor: 0,
            preview: None,
            editor_request: None,
            pick_request: None,
            notice: None,
            dvc_lookup: runner::find_dvc_in_env,
            should_quit: false,
        }
    }

    /// Load the run times remembered from earlier sessions and keep saving new ones.
    /// Off by default, so tests never touch a fixture's `.dvc` directory.
    pub fn enable_history(&mut self) {
        let remembered = history::load(&self.project.root);
        for (stage, run) in remembered.stages {
            self.last_runs.insert(
                stage,
                LastRun {
                    outcome: run.outcome.to_outcome(),
                    duration: Duration::from_secs_f64(run.seconds),
                },
            );
        }
        self.experiment_times.extend(remembered.experiments);
        self.experiments.hidden = remembered.hidden_params;
        self.persist_history = true;
    }

    fn save_history(&self) {
        if !self.persist_history {
            return;
        }
        let stages = self
            .last_runs
            .iter()
            .filter_map(|(stage, run)| {
                let outcome = StoredOutcome::from_outcome(&run.outcome)?;
                Some((
                    stage.clone(),
                    StoredRun {
                        outcome,
                        seconds: run.duration.as_secs_f64(),
                    },
                ))
            })
            .collect();
        let history = History {
            stages,
            experiments: self.experiment_times.clone(),
            hidden_params: self.experiments.hidden.clone(),
        };
        // A history that cannot be written is not worth interrupting the user for.
        let _ = history::save(&self.project.root, &history);
    }

    /// Put the remembered experiment run times into a freshly loaded table.
    fn apply_experiment_times(&self, table: &mut Table) {
        for row in &mut table.rows {
            if row.kind == RowKind::Experiment {
                row.run_secs = self.experiment_times.get(&row.label).copied();
            }
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

    /// The files of the selected stage, in the order the files pane shows them.
    pub fn selected_files(&self) -> Vec<FileEntry> {
        self.selected_stage()
            .map(files::stage_files)
            .unwrap_or_default()
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
            Event::Remotes(transfer, result) => {
                self.offer_remotes(transfer, result);
                Vec::new()
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
            Event::PreviewLoaded(preview) => {
                self.preview = Some(preview);
                Vec::new()
            }
            Event::Experiments(mut result) => {
                if let Ok(table) = &mut result {
                    self.apply_experiment_times(table);
                }
                self.experiments.loading = false;
                self.experiments.data = Some(result);
                self.clamp_experiments_cursor();
                Vec::new()
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // Raw mode turns off the terminal's own Ctrl+C handling, so we do it ourselves.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Vec::new();
        }
        if self.form.is_some() {
            return self.on_form_key(key);
        }
        if self.plot_form.is_some() {
            return self.on_plot_form_key(key);
        }
        if self.stage_form.is_some() {
            return self.on_stage_form_key(key);
        }
        if self.confirm.is_some() {
            return self.on_confirm_key(key.code);
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
        }
        if self.param_picker.is_some() {
            return self.on_picker_key(key);
        }
        if self.remote_picker.is_some() {
            return self.on_remote_picker_key(key);
        }
        // An open popup is modal: it gets every key, so `q` closes it instead of quitting.
        if self.preview.is_some() {
            return self.on_preview_key(key.code);
        }
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                Vec::new()
            }
            KeyCode::Char('e') => self.toggle_view(),
            KeyCode::Char('1') => {
                self.view = View::Pipeline;
                Vec::new()
            }
            KeyCode::Char('2') => {
                self.view = View::Dag;
                Vec::new()
            }
            KeyCode::Char('3') if self.view != View::Experiments => self.toggle_view(),
            KeyCode::Char('4') => self.open_plots(),
            KeyCode::Char('p') if self.view != View::Experiments => self.open_plots(),
            KeyCode::Char('g') => {
                self.view = if self.view == View::Dag {
                    View::Pipeline
                } else {
                    View::Dag
                };
                Vec::new()
            }
            KeyCode::Char('P') => self.request_remotes(Transfer::Pull),
            KeyCode::Char('U') => self.request_remotes(Transfer::Push),
            // Stopping a run makes sense from either view.
            KeyCode::Char('x') => self.request_cancel(),
            KeyCode::Char('X') => {
                self.ask_to_stop_external();
                Vec::new()
            }
            code => match self.view {
                View::Pipeline => self.on_pipeline_key(code),
                View::Experiments => self.on_experiments_key(code),
                View::Dag => self.on_dag_key(code),
                View::Plots => self.on_plots_key(code),
            },
        }
    }

    fn on_pipeline_key(&mut self, code: KeyCode) -> Vec<Effect> {
        match code {
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Stages => self.move_selection_down(),
                Focus::Files => self.move_file_cursor(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Stages => self.move_selection_up(),
                Focus::Files => self.move_file_cursor(-1),
            },
            KeyCode::Enter => match self.focus {
                // Enter on a stage steps into its files; Enter on a file opens the preview.
                Focus::Stages => self.focus = Focus::Files,
                Focus::Files => return self.preview_effect(Effect::LoadPreview),
            },
            KeyCode::Char('o') if self.focus == Focus::Files => {
                return self.preview_effect(Effect::OpenEditor);
            }
            KeyCode::Esc if self.focus == Focus::Files => self.focus = Focus::Stages,
            KeyCode::Char('r') => return self.request_run(false),
            KeyCode::Char('R') => return self.request_run(true),
            KeyCode::PageUp => self.scroll_log_back(),
            KeyCode::PageDown => self.scroll_log_forward(),
            KeyCode::Char('n') => self.stage_form = Some(StageForm::default()),
            _ => {}
        }
        Vec::new()
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Stages => Focus::Files,
            Focus::Files => Focus::Stages,
        };
    }

    fn move_file_cursor(&mut self, delta: isize) {
        let last = self.selected_files().len().saturating_sub(1);
        self.file_cursor = self.file_cursor.saturating_add_signed(delta).min(last);
    }

    /// Build an effect for the file under the cursor (nothing if the stage has no files).
    fn preview_effect(&self, make: fn(String) -> Effect) -> Vec<Effect> {
        match self.selected_files().into_iter().nth(self.file_cursor) {
            Some(entry) => vec![make(entry.path)],
            None => Vec::new(),
        }
    }

    fn on_preview_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let Some(preview) = &mut self.preview else {
            return Vec::new();
        };
        let last = preview.line_count().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.preview = None,
            KeyCode::Char('j') | KeyCode::Down => preview.scroll = (preview.scroll + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => preview.scroll = preview.scroll.saturating_sub(1),
            KeyCode::PageDown => preview.scroll = (preview.scroll + PREVIEW_PAGE).min(last),
            KeyCode::PageUp => preview.scroll = preview.scroll.saturating_sub(PREVIEW_PAGE),
            KeyCode::Char('g') | KeyCode::Home => preview.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => preview.scroll = last,
            KeyCode::Char('o') => return vec![Effect::OpenEditor(preview.path.clone())],
            _ => {}
        }
        Vec::new()
    }

    /// Keys in the graph view: move along edges (`h`/`l`) and within a layer (`j`/`k`).
    fn on_dag_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let movement = match code {
            KeyCode::Char('h') | KeyCode::Left => Some(Move::Upstream),
            KeyCode::Char('l') | KeyCode::Right => Some(Move::Downstream),
            KeyCode::Char('k') | KeyCode::Up => Some(Move::Up),
            KeyCode::Char('j') | KeyCode::Down => Some(Move::Down),
            _ => None,
        };
        if let Some(movement) = movement {
            if let Some(&stage) = self.graph.order.get(self.selected) {
                let target = self.dag.step(stage, movement);
                if let Some(position) = self.graph.order.iter().position(|&s| s == target)
                    && position != self.selected
                {
                    self.selected = position;
                    self.file_cursor = 0;
                }
            }
            return Vec::new();
        }
        match code {
            // The graph view shares the selection with the pipeline view, so Enter just goes there.
            KeyCode::Enter | KeyCode::Esc => self.view = View::Pipeline,
            KeyCode::Char('n') => self.stage_form = Some(StageForm::default()),
            KeyCode::Char('r') => return self.request_run(false),
            KeyCode::Char('R') => return self.request_run(true),
            _ => {}
        }
        Vec::new()
    }

    fn on_stage_form_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(form) = &mut self.stage_form else {
            return Vec::new();
        };
        match form.on_key(key) {
            StageFormAction::Continue => {}
            StageFormAction::Cancel => self.stage_form = None,
            StageFormAction::Pick(pick) => {
                let items = self.pick_candidates(pick);
                if let Some(form) = &mut self.stage_form {
                    if items.is_empty() {
                        form.error = Some("nothing to pick from".to_owned());
                    } else {
                        self.pick_request = Some(PickRequest {
                            items,
                            multi: true,
                            prompt: format!("{pick:?} (Tab: several)").to_lowercase(),
                            target: PickTarget::Stage(pick),
                        });
                    }
                }
            }
            StageFormAction::Submit(draft) => match self.add_stage(&draft) {
                Ok(()) => {
                    self.stage_form = None;
                    self.notice = Some(format!("added stage {}", draft.name));
                    return vec![Effect::RefreshStatuses];
                }
                Err(message) => {
                    if let Some(form) = &mut self.stage_form {
                        form.error = Some(message);
                    }
                }
            },
        }
        Vec::new()
    }

    /// What `fzf` offers for a list field of the stage form.
    fn pick_candidates(&self, pick: Pick) -> Vec<String> {
        match pick {
            Pick::Params => self.param_candidates(),
            Pick::Deps | Pick::Outs | Pick::Metrics => {
                dvc_yaml::iter_files(&self.project.root, |name, _| {
                    name != "dvc.lock" && !name.ends_with(".pyc")
                })
                .take(MAX_PICK_FILES)
                .collect()
            }
        }
    }

    /// Every param key of `params.yaml` and of the other param files the stages use: a plain key
    /// for the default file, `file:key` for the rest.
    fn param_candidates(&self) -> Vec<String> {
        let mut files: Vec<&str> = vec![DEFAULT_PARAMS_FILE];
        files.extend(
            self.project
                .params
                .keys()
                .map(String::as_str)
                .filter(|file| *file != DEFAULT_PARAMS_FILE),
        );
        let mut keys = Vec::new();
        for file in files {
            let Ok(tree) = project::load_param_file(&self.project.root, file) else {
                continue;
            };
            for (key, _) in project::flatten_leaves("", &tree) {
                let key = key.trim_start_matches('.');
                keys.push(if file == DEFAULT_PARAMS_FILE {
                    key.to_owned()
                } else {
                    format!("{file}:{key}")
                });
            }
        }
        keys
    }

    /// Write the stage into `dvc.yaml` and load the stages again. Nothing is written when the
    /// result would not parse or would make a dependency cycle.
    fn add_stage(&mut self, draft: &stage_form::StageDraft) -> Result<(), String> {
        if self
            .project
            .stages
            .iter()
            .any(|stage| stage.name == draft.name)
        {
            return Err(format!("a stage named {} already exists", draft.name));
        }
        let root = self.project.root.clone();
        let path = root.join("dvc.yaml");
        let text = fs::read_to_string(&path).map_err(|e| format!("cannot read dvc.yaml: {e}"))?;
        let updated = stage_form::add_to_dvc_yaml(&text, draft)?;
        let stages =
            project::parse_dvc_yaml_in(&updated, Some(&root)).map_err(|e| format!("{e:#}"))?;
        graph::build(&stages).map_err(|e| e.to_string())?;
        fs::write(&path, updated).map_err(|e| format!("cannot write dvc.yaml: {e}"))?;
        self.reload_stages(&draft.name)
    }

    /// Read `dvc.yaml` again, rebuild the graph and select `name`. Stale statuses are dropped:
    /// the caller starts a new check.
    fn reload_stages(&mut self, name: &str) -> Result<(), String> {
        let project = Project::load(&self.project.root).map_err(|e| format!("{e:#}"))?;
        let graph = graph::build(&project.stages).map_err(|e| e.to_string())?;
        let names: Vec<&str> = project.stages.iter().map(|s| s.name.as_str()).collect();
        self.dag = dag::layout(&names, &graph);
        let index = project.stages.iter().position(|s| s.name == name);
        self.selected = index
            .and_then(|i| graph.order.iter().position(|&o| o == i))
            .unwrap_or(0);
        self.file_cursor = 0;
        self.statuses = None;
        self.project = project;
        self.graph = graph;
        Ok(())
    }

    fn open_plots(&mut self) -> Vec<Effect> {
        self.view = View::Plots;
        if self.plots.data.is_none() {
            self.load_plots();
        }
        Vec::new()
    }

    /// Read `dvc.yaml` and the plot data files again. They are small, so this happens on the spot.
    fn load_plots(&mut self) {
        let data = plots::load(&self.project.root);
        let count = data.as_ref().map_or(0, Vec::len);
        self.plots.selected = self.plots.selected.min(count.saturating_sub(1));
        self.plots.data = Some(data);
    }

    fn on_plots_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let count = match &self.plots.data {
            Some(Ok(list)) => list.len(),
            _ => 0,
        };
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.plots.selected = (self.plots.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.plots.selected = self.plots.selected.saturating_sub(1);
            }
            KeyCode::Char('d') => self.plots.definition = !self.plots.definition,
            KeyCode::Char('n') => self.plot_form = Some(PlotForm::default()),
            KeyCode::F(5) => self.load_plots(),
            KeyCode::Esc => self.view = View::Pipeline,
            _ => {}
        }
        Vec::new()
    }

    fn on_plot_form_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(form) = &mut self.plot_form else {
            return Vec::new();
        };
        match form.on_key(key) {
            PlotFormAction::Continue => {}
            PlotFormAction::Cancel => self.plot_form = None,
            PlotFormAction::PickFile => {
                let files: Vec<String> = plots::iter_data_files(&self.project.root)
                    .take(MAX_PICK_FILES)
                    .collect();
                if files.is_empty() {
                    form.error = Some("no csv, tsv, json or yaml files in the project".to_owned());
                } else {
                    self.pick_request = Some(PickRequest {
                        items: files,
                        multi: false,
                        prompt: "plot data".to_owned(),
                        target: PickTarget::PlotFile,
                    });
                }
            }
            PlotFormAction::Submit(draft) => match self.add_plot(&draft) {
                Ok(()) => {
                    self.plot_form = None;
                    self.notice = Some(format!("added plot {}", draft.file));
                }
                Err(message) => {
                    if let Some(form) = &mut self.plot_form {
                        form.error = Some(message);
                    }
                }
            },
        }
        Vec::new()
    }

    /// Write the plot into `dvc.yaml`, read the plots again and select the new one.
    fn add_plot(&mut self, draft: &plots::Draft) -> Result<(), String> {
        if let Some(Ok(existing)) = &self.plots.data
            && existing.iter().any(|plot| plot.id == draft.file)
        {
            return Err(format!("a plot named {} already exists", draft.file));
        }
        let path = self.project.root.join("dvc.yaml");
        let text = fs::read_to_string(&path).map_err(|e| format!("cannot read dvc.yaml: {e}"))?;
        let updated = plots::add_to_dvc_yaml(&text, draft)?;
        fs::write(&path, updated).map_err(|e| format!("cannot write dvc.yaml: {e}"))?;
        self.load_plots();
        if let Some(Ok(list)) = &self.plots.data
            && let Some(index) = list.iter().position(|plot| plot.id == draft.file)
        {
            self.plots.selected = index;
        }
        Ok(())
    }

    fn toggle_view(&mut self) -> Vec<Effect> {
        match self.view {
            View::Experiments => {
                self.view = View::Pipeline;
                Vec::new()
            }
            View::Pipeline | View::Dag | View::Plots => {
                self.view = View::Experiments;
                // Fetch on first visit only; F5 refreshes after that.
                if self.experiments.data.is_none() {
                    self.request_experiments()
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn request_experiments(&mut self) -> Vec<Effect> {
        if self.experiments.loading {
            return Vec::new();
        }
        if self.is_running() {
            self.notice = Some(
                "experiments can't be loaded while a stage runs (DVC holds the repository lock); press F5 afterwards"
                    .to_owned(),
            );
            return Vec::new();
        }
        self.experiments.loading = true;
        vec![Effect::FetchExperiments]
    }

    fn on_experiments_key(&mut self, code: KeyCode) -> Vec<Effect> {
        // Transposed, the arrows follow the screen: down/up walk the fields, left/right the experiments.
        let code = match (self.experiments.transpose, code) {
            (true, KeyCode::Char('j')) => KeyCode::Char('l'),
            (true, KeyCode::Down) => KeyCode::Right,
            (true, KeyCode::Char('k')) => KeyCode::Char('h'),
            (true, KeyCode::Up) => KeyCode::Left,
            (true, KeyCode::Char('l')) => KeyCode::Char('j'),
            (true, KeyCode::Right) => KeyCode::Down,
            (true, KeyCode::Char('h')) => KeyCode::Char('k'),
            (true, KeyCode::Left) => KeyCode::Up,
            (_, code) => code,
        };
        match code {
            KeyCode::Char('t') => self.experiments.transpose = !self.experiments.transpose,
            KeyCode::Char('j') | KeyCode::Down => self.experiments.row += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.experiments.row = self.experiments.row.saturating_sub(1);
            }
            KeyCode::Char('l') | KeyCode::Right => self.experiments.column += 1,
            KeyCode::Char('h') | KeyCode::Left => {
                self.experiments.column = self.experiments.column.saturating_sub(1);
            }
            KeyCode::Char('s') => self.sort_by_cursor_column(),
            KeyCode::Char('m') => self.experiments.deltas = !self.experiments.deltas,
            KeyCode::Char('p') => self.experiments.plot = !self.experiments.plot,
            KeyCode::Char('[') => self.cycle_plot_metric(false),
            KeyCode::Char(']') => self.cycle_plot_metric(true),
            KeyCode::Char('v') => self.open_param_picker(),
            KeyCode::Char('H') => self.hide_cursor_param(),
            KeyCode::Char('n') => self.open_experiment_form(),
            KeyCode::Char('b') => self.open_branch_prompt(),
            KeyCode::Char('c') => self.open_comment_prompt(),
            KeyCode::Char('a') => self.ask_about_selected_experiment(Action::Apply),
            KeyCode::Char('d') => self.ask_about_selected_experiment(Action::Remove),
            KeyCode::F(5) => return self.request_experiments(),
            KeyCode::Esc => self.view = View::Pipeline,
            _ => {}
        }
        self.clamp_experiments_cursor();
        Vec::new()
    }

    fn open_param_picker(&mut self) {
        match self.experiments.table() {
            Some(table) if !table.param_columns.is_empty() => {
                self.param_picker = Some(ParamPicker::new(&table.param_columns));
            }
            _ => self.notice = Some("there are no params to choose from".to_owned()),
        }
    }

    fn on_picker_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(mut picker) = self.param_picker.take() {
            match picker.on_key(key, &mut self.experiments.hidden) {
                PickerAction::Continue => self.param_picker = Some(picker),
                PickerAction::Changed => {
                    self.param_picker = Some(picker);
                    self.save_history();
                    self.clamp_experiments_cursor();
                }
                PickerAction::Close => {}
            }
        }
        Vec::new()
    }

    /// `H`: hide the param column under the cursor (`v` brings it back).
    fn hide_cursor_param(&mut self) {
        let Some(table) = self.experiments.table() else {
            return;
        };
        match self.experiments.columns(table).get(self.experiments.column) {
            Some(experiments::Column::Param(label)) => {
                let label = label.clone();
                self.experiments.hidden.insert(label);
                self.notice = Some("param hidden; v lists and restores hidden params".to_owned());
                self.save_history();
            }
            _ => self.notice = Some("H hides the param column under the cursor".to_owned()),
        }
    }

    fn cycle_plot_metric(&mut self, forward: bool) {
        let count = self
            .experiments
            .table()
            .map_or(0, |table| crate::plot::plottable_metrics(table).len());
        if count == 0 {
            return;
        }
        let current = self.experiments.plot_metric % count;
        self.experiments.plot_metric = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
    }

    /// Open the form with the values of the row under the cursor.
    fn open_experiment_form(&mut self) {
        if self.is_running() {
            self.notice = Some("a run is already in progress (x cancels it)".to_owned());
            return;
        }
        let Some(table) = self.experiments.table() else {
            self.notice = Some("experiments are not available (press F5 to load them)".to_owned());
            return;
        };
        let rows = table.ordered(&self.experiments.sort);
        let row = rows.get(self.experiments.row).copied();
        self.form = Some(ExperimentForm::from_table(table, row));
    }

    /// `P` / `U`: look up the remotes; [`App::offer_remotes`] continues when they are known.
    fn request_remotes(&mut self, transfer: Transfer) -> Vec<Effect> {
        if self.is_running() {
            self.notice = Some("a run is already in progress (x cancels it)".to_owned());
            return Vec::new();
        }
        self.notice = Some("looking up the remotes…".to_owned());
        vec![Effect::LoadRemotes(transfer)]
    }

    /// The remotes are known: no remote is an error, one is asked about directly, several are
    /// listed to choose from.
    fn offer_remotes(&mut self, transfer: Transfer, result: Result<Vec<Remote>, String>) {
        self.notice = None;
        match result {
            Err(error) => self.notice = Some(error),
            Ok(remotes) if remotes.is_empty() => {
                self.notice = Some("no remote is configured (dvc remote add -d NAME URL)".into());
            }
            Ok(mut remotes) if remotes.len() == 1 => {
                self.ask_about_transfer(transfer, remotes.remove(0));
            }
            Ok(remotes) => self.remote_picker = Some(RemotePicker::new(transfer, remotes)),
        }
    }

    fn on_remote_picker_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(mut picker) = self.remote_picker.take() {
            match picker.on_key(key) {
                RemoteChoice::Continue => self.remote_picker = Some(picker),
                RemoteChoice::Cancel => {}
                RemoteChoice::Chosen(remote) => self.ask_about_transfer(picker.transfer, remote),
            }
        }
        Vec::new()
    }

    /// Ask before pulling from or pushing to `remote`.
    fn ask_about_transfer(&mut self, transfer: Transfer, remote: Remote) {
        let Remote { name, url, .. } = remote;
        let (title, what, label) = match transfer {
            Transfer::Pull => (
                format!("Pull from {name}?"),
                "Downloads the data that dvc.lock and the .dvc files point at. Workspace files that differ from the cache are overwritten.",
                format!("dvc pull -r {name}"),
            ),
            Transfer::Push => (
                format!("Push to {name}?"),
                "Uploads the cached outputs of dvc.lock and the .dvc files. Other people with access to the remote can then pull them.",
                format!("dvc push -r {name}"),
            ),
        };
        // `dvc pull` / `dvc push` take stage names as targets, so offer the selected stage.
        let stage = matches!(self.view, View::Pipeline | View::Dag)
            .then(|| self.selected_stage())
            .flatten()
            .filter(|stage| stage.unsupported.is_none())
            .map(|stage| stage.name.clone());
        let mut toggles = Vec::new();
        if let Some(stage) = stage {
            toggles.push(Toggle {
                key: 't',
                text: format!("only the data of stage {stage}"),
                on: false,
                args: vec![stage],
            });
        }
        toggles.push(Toggle {
            key: 'c',
            text: "include the run-cache (--run-cache)".to_owned(),
            on: false,
            args: vec!["--run-cache".to_owned()],
        });
        self.confirm = Some(Confirm {
            title,
            lines: vec![format!("Remote {name}: {url}"), what.to_owned()],
            label,
            args: remotes::transfer_args(transfer, &name),
            kill: Vec::new(),
            toggles,
        });
    }

    /// `a` / `d`: ask before applying or removing the experiment under the cursor.
    fn ask_about_selected_experiment(&mut self, action: Action) {
        let Some(name) = self.selected_experiment_for_action() else {
            return;
        };
        self.confirm = Some(match action {
            Action::Apply => Confirm {
                title: format!("Apply {name}?"),
                lines: vec![
                    "Overwrites params, lock file, metrics and outputs in your workspace.".into(),
                    "Your uncommitted changes are saved first. To undo: git reset --hard && git stash apply refs/exps/apply/stash".into(),
                ],
                label: format!("apply {name}"),
                args: runner::exp_apply_args(&name),
                kill: Vec::new(),
                toggles: Vec::new(),
            },
            Action::Remove => Confirm {
                title: format!("Remove {name}?"),
                lines: vec![
                    "Deletes the experiment from this repository.".into(),
                    "This cannot be undone (unless it was pushed to a remote).".into(),
                ],
                label: format!("remove {name}"),
                args: runner::exp_remove_args(&name),
                kill: Vec::new(),
                toggles: Vec::new(),
            },
        });
    }

    /// The name of the experiment under the cursor, when an action on it is allowed right now.
    /// Otherwise sets a notice that says why and returns `None`.
    fn selected_experiment_for_action(&mut self) -> Option<String> {
        if self.is_running() {
            self.notice = Some("a run is already in progress (x cancels it)".to_owned());
            return None;
        }
        let Some(table) = self.experiments.table() else {
            self.notice = Some("experiments are not available (press F5 to load them)".to_owned());
            return None;
        };
        let rows = table.ordered(&self.experiments.sort);
        let name = rows
            .get(self.experiments.row)
            .filter(|row| row.kind == RowKind::Experiment)
            .map(|row| row.label.clone());
        if name.is_none() {
            self.notice = Some(
                "select an experiment row first (not the workspace or the baseline)".to_owned(),
            );
        }
        name
    }

    fn open_branch_prompt(&mut self) {
        if let Some(name) = self.selected_experiment_for_action() {
            self.prompt = Some(Prompt::branch(&name));
        }
    }

    fn open_comment_prompt(&mut self) {
        let Some(table) = self.experiments.table() else {
            self.notice = Some("experiments are not available (press F5 to load them)".to_owned());
            return;
        };
        let rows = table.ordered(&self.experiments.sort);
        let Some(row) = rows
            .get(self.experiments.row)
            .filter(|row| row.kind == RowKind::Experiment)
        else {
            self.notice = Some(
                "select an experiment row first (not the workspace or the baseline)".to_owned(),
            );
            return;
        };
        let subject = table.message_of(row).map_or("", messages::subject);
        self.prompt = Some(Prompt::comment(&row.label, &row.rev, subject));
    }

    fn on_prompt_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let running = self.is_running();
        let Some(prompt) = &mut self.prompt else {
            return Vec::new();
        };
        match prompt.on_key(key) {
            PromptAction::Continue => Vec::new(),
            PromptAction::Cancel => {
                self.prompt = None;
                Vec::new()
            }
            PromptAction::Submit(text) => {
                if let PromptKind::Comment { rev } = prompt.kind.clone() {
                    let old = self
                        .experiments
                        .table()
                        .and_then(|table| table.messages.get(&rev))
                        .cloned()
                        .unwrap_or_default();
                    self.prompt = None;
                    let message = messages::with_subject(&old, &text);
                    return vec![Effect::SaveMessage { rev, message }];
                }
                let branch = text;
                if running {
                    prompt.error = Some("a run is already in progress (x cancels it)".to_owned());
                    return Vec::new();
                }
                let experiment = prompt.target.clone();
                self.prompt = None;
                let args = runner::exp_branch_args(&experiment, &branch);
                self.run = Some(Run::branch(format!("branch {branch}"), args, branch));
                self.view = View::Pipeline;
                self.notice = Some("creating the branch; the log is below".to_owned());
                vec![Effect::StartExperiment]
            }
        }
    }

    fn on_confirm_key(&mut self, code: KeyCode) -> Vec<Effect> {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => match self.confirm.take() {
                Some(Confirm { kill, .. }) if !kill.is_empty() => {
                    self.notice = Some("stopping the other dvc process…".to_owned());
                    vec![Effect::StopExternal(kill)]
                }
                Some(confirm) => {
                    let (label, args) = confirm.command();
                    self.begin_dvc_run(label, args)
                }
                None => Vec::new(),
            },
            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                self.confirm = None;
                Vec::new()
            }
            KeyCode::Char(key) => {
                if let Some(toggle) = self
                    .confirm
                    .iter_mut()
                    .flat_map(|confirm| confirm.toggles.iter_mut())
                    .find(|toggle| toggle.key == key)
                {
                    toggle.on = !toggle.on;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Start a `dvc` command that works on experiments, showing its log in the pipeline view.
    /// When it ends the experiments table is reloaded (see `on_run_finished`).
    fn begin_dvc_run(&mut self, label: String, args: Vec<String>) -> Vec<Effect> {
        let remote = matches!(args.first().map(String::as_str), Some("pull" | "push"));
        self.run = Some(if remote {
            Run::remote(label, args)
        } else {
            Run::experiment(label, args)
        });
        // The log pane lives in the pipeline view, so show it.
        self.view = View::Pipeline;
        self.notice = Some("running; press e for the table when it ends".to_owned());
        vec![Effect::StartExperiment]
    }

    fn on_form_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // Ask before borrowing `self.form` mutably: `is_running` borrows all of `self`.
        let running = self.is_running();
        let Some(form) = &mut self.form else {
            return Vec::new();
        };
        match form.on_key(key) {
            FormAction::Continue => Vec::new(),
            FormAction::Cancel => {
                self.form = None;
                Vec::new()
            }
            FormAction::Submit { label, args } => {
                if running {
                    form.error = Some("a run is already in progress (x cancels it)".to_owned());
                    return Vec::new();
                }
                self.form = None;
                self.begin_dvc_run(label, args)
            }
        }
    }

    fn sort_by_cursor_column(&mut self) {
        let Some(table) = self.experiments.table() else {
            return;
        };
        let Some(column) = self
            .experiments
            .columns(table)
            .into_iter()
            .nth(self.experiments.column)
        else {
            return;
        };
        self.experiments.sort.choose(column);
    }

    /// Keep the cursor on an existing row and column (the table may have shrunk or be absent).
    fn clamp_experiments_cursor(&mut self) {
        let (rows, columns) = self.experiments.table().map_or((0, 0), |table| {
            (table.rows.len(), self.experiments.columns(table).len())
        });
        self.experiments.row = self.experiments.row.min(rows.saturating_sub(1));
        self.experiments.column = self.experiments.column.min(columns.saturating_sub(1));
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
        if let Some(reason) = self.blocked_by_outside_run(with_upstream) {
            self.notice = Some(format!("cannot run '{name}': {reason} (X stops it)"));
            return Vec::new();
        }
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
        let kind = run.kind.clone();
        let succeeded = outcome == RunOutcome::Succeeded;
        let duration = run.fixed_duration.unwrap_or_else(|| run.started.elapsed());
        run.elapsed = Some(duration);
        // A run in which DVC only said "didn't change, skipping" did nothing, so it is no "last run".
        let skipped = run.skipped_target();
        if kind == RunKind::Stage && !skipped && !matches!(outcome, RunOutcome::NotStarted(_)) {
            let stage = run.stage.clone();
            self.last_runs.insert(
                stage,
                LastRun {
                    outcome: outcome.clone(),
                    duration,
                },
            );
        }
        // `exp run` prints "Ran experiment(s): <names>"; those names get this run's duration.
        let ran_experiments = match (&kind, run.args.get(1).map(String::as_str)) {
            (RunKind::Experiment, Some("run")) if succeeded => {
                history::experiments_named_in(run.log.lines().map(str::to_owned))
            }
            _ => Vec::new(),
        };
        for name in ran_experiments {
            self.experiment_times.insert(name, duration.as_secs_f64());
        }
        self.save_history();
        let blocked = matches!(outcome, RunOutcome::Failed { .. })
            .then(|| self.blocking_runs())
            .unwrap_or_default();
        if let Some(run) = &mut self.run {
            run.phase = RunPhase::Finished(outcome);
        }
        if !blocked.is_empty() {
            self.confirm = Some(blocked_confirm(&blocked));
        }
        let mut effects = Vec::new();
        // A new branch changes nothing in the workspace, so nothing needs re-checking.
        let refresh = refresh && !matches!(kind, RunKind::Branch { .. });
        if let RunKind::Branch { name } = &kind
            && succeeded
        {
            self.notice = Some(format!(
                "branch '{name}' created (not checked out); switch with: git switch {name}"
            ));
        }
        if refresh {
            // Whatever ran may have rewritten dvc.lock, so earlier results are out of date.
            self.statuses = None;
            effects.push(Effect::RefreshStatuses);
            if kind == RunKind::Experiment {
                // A new row may have appeared. (The lock is free again: the run is over.)
                effects.extend(self.request_experiments());
            }
        }
        effects
    }

    /// Called every time the main loop wakes up: look again for outside `dvc` runs, at most once
    /// per [`EXTERNAL_CHECK_INTERVAL`].
    pub fn tick(&mut self) {
        if self
            .external_checked
            .is_some_and(|at| at.elapsed() < EXTERNAL_CHECK_INTERVAL)
        {
            return;
        }
        self.external_checked = Some(Instant::now());
        self.external = external::iter_external_runs(&self.project.root).collect();
    }

    /// The outside run working on the stage at `position`, if any.
    pub fn external_on(&self, position: usize) -> Option<&external::ExternalRun> {
        let stage = self.stage_at(position)?;
        self.external
            .iter()
            .find(|run| run.works_on(&self.project.root, &stage.name, stage.produced()))
    }

    /// Would DVC refuse to lock what the selected stage (and, with `with_upstream`, the stages it
    /// depends on) needs, because an outside run holds it? Looks at the lock file afresh.
    fn blocked_by_outside_run(&mut self, with_upstream: bool) -> Option<String> {
        self.tick_now();
        if self.external.is_empty() {
            return None;
        }
        let mut names: Vec<&str> = vec![self.selected_stage()?.name.as_str()];
        if with_upstream {
            names.extend(self.selected_upstream());
        }
        let root = &self.project.root;
        for stage in self
            .project
            .stages
            .iter()
            .filter(|s| names.contains(&s.name.as_str()))
        {
            let deps = stage
                .deps
                .iter()
                .map(String::as_str)
                .chain(stage.params.iter().map(|p| p.file.as_str()));
            for run in &self.external {
                if let Some(why) = run.conflict_with(root, deps.clone(), stage.produced()) {
                    return Some(if stage.name == names[0] {
                        why
                    } else {
                        format!("upstream '{}': {why}", stage.name)
                    });
                }
            }
        }
        None
    }

    /// `X`: ask whether to stop the outside runs that are holding the project.
    fn ask_to_stop_external(&mut self) {
        self.tick_now();
        if self.external.is_empty() {
            self.notice = Some("no dvc process outside dvcat is running here".to_owned());
        } else {
            self.confirm = Some(blocked_confirm(&self.external));
        }
    }

    fn tick_now(&mut self) {
        self.external_checked = None;
        self.tick();
    }

    /// If the finished run failed because other `dvc` processes hold the lock, those processes.
    fn blocking_runs(&self) -> Vec<external::ExternalRun> {
        let Some(run) = &self.run else {
            return Vec::new();
        };
        let log: String = run.log.lines().collect::<Vec<_>>().join("\n");
        if !external::log_mentions_lock(&log) {
            return Vec::new();
        }
        external::iter_external_runs(&self.project.root).collect()
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
        // Once the end of a run has been seen, its files are no longer needed: without this, the
        // next session would find the finished run and report it again.
        if matches!(event, Event::RunFinished(_)) {
            runner::forget(&self.project.root);
        }
        for effect in self.update(event) {
            self.execute(effect, tx);
        }
    }

    fn execute(&mut self, effect: Effect, tx: &Sender<Event>) {
        match effect {
            Effect::StartRun {
                stage,
                with_upstream,
            } => {
                let _ = (stage, with_upstream); // already stored in `self.run.args`
                self.start_job(tx);
            }
            Effect::StartExperiment => self.start_job(tx),
            Effect::CancelRun => {
                if let Some(job) = self.run.as_ref().and_then(|run| run.job.as_ref()) {
                    job.cancel();
                }
            }
            Effect::StopExternal(pids) => {
                // Waiting for the processes to exit can take seconds, so the interface must not.
                thread::spawn(move || external::stop(&pids, external::KILL_GRACE));
            }
            Effect::FetchExperiments => self.fetch_experiments(tx),
            Effect::LoadRemotes(transfer) => self.fetch_remotes(transfer, tx),
            Effect::SaveMessage { rev, message } => {
                match messages::rewrite(&self.project.root, &rev, &message) {
                    Ok(_) => {
                        self.notice = Some("message saved".to_owned());
                        self.experiments.loading = true;
                        self.fetch_experiments(tx);
                    }
                    Err(error) => {
                        self.notice = Some(format!("could not save the message: {error}"))
                    }
                }
            }
            Effect::LoadPreview(path) => {
                // The read is capped (see `files::PREVIEW_LIMIT`), so doing it here is fast.
                let body = files::load(&self.project.root, &path);
                self.dispatch(Event::PreviewLoaded(Preview::new(path, body)), tx);
            }
            Effect::OpenEditor(path) => {
                let full = self.project.root.join(&path);
                if full.exists() {
                    self.editor_request = Some(full);
                } else {
                    self.notice = Some(format!("{path} does not exist yet"));
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

    /// The editor has exited. Show its error (if any) and re-check the stages: the user may have
    /// changed `params.yaml` or a source file.
    pub fn editor_closed(&mut self, error: Option<String>, tx: &Sender<Event>) {
        self.execute(Effect::RefreshStatuses, tx);
        // After the refresh, so a reload problem does not hide the editor's own error.
        if error.is_some() {
            self.notice = error;
        }
    }

    /// `fzf` has exited: an empty list means nothing was chosen. A choice goes into the form
    /// field the pick was for.
    pub fn file_picked(&mut self, target: PickTarget, result: Result<Vec<String>, String>) {
        let error = match (target, result) {
            (_, Ok(chosen)) if chosen.is_empty() => return,
            (PickTarget::PlotFile, Ok(chosen)) => {
                if let (Some(form), Some(file)) = (&mut self.plot_form, chosen.first()) {
                    form.set_file(file);
                }
                return;
            }
            (PickTarget::Stage(pick), Ok(chosen)) => {
                if let Some(form) = &mut self.stage_form {
                    form.add_picked(pick, &chosen);
                }
                return;
            }
            (_, Err(message)) => message,
        };
        if let Some(form) = &mut self.plot_form {
            form.error = Some(error);
        } else if let Some(form) = &mut self.stage_form {
            form.error = Some(error);
        }
    }

    fn fetch_remotes(&mut self, transfer: Transfer, tx: &Sender<Event>) {
        let root = self.project.root.clone();
        match (self.dvc_lookup)(&root) {
            Ok(dvc) => {
                let tx = tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(Event::Remotes(transfer, remotes::fetch(&dvc, &root)));
                });
            }
            Err(error) => self.dispatch(Event::Remotes(transfer, Err(error.to_string())), tx),
        }
    }

    fn fetch_experiments(&mut self, tx: &Sender<Event>) {
        let root = self.project.root.clone();
        match (self.dvc_lookup)(&root) {
            Ok(dvc) => {
                let tx = tx.clone();
                thread::spawn(move || {
                    let result = experiments::fetch(&dvc, &root);
                    let _ = tx.send(Event::Experiments(result));
                });
            }
            // Report it the same way as any other failed fetch.
            Err(error) => self.dispatch(Event::Experiments(Err(error.to_string())), tx),
        }
    }

    /// Start the process described by the current `self.run`.
    fn start_job(&mut self, tx: &Sender<Event>) {
        let root = self.project.root.clone();
        let Some(record) = self.run.as_ref().map(Run::record) else {
            return;
        };
        let started = (self.dvc_lookup)(&root)
            .map_err(|error| error.to_string())
            .and_then(|dvc| {
                runner::start_recorded(&dvc, &root, record, tx.clone())
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
    ///
    /// A run keeps going after the interface quits (the next session picks it up again), with one
    /// exception: a run that is being cancelled is waited for, because the timer that escalates
    /// to SIGKILL dies with this process. Returns a message when a run is left running.
    pub fn shutdown(&mut self) -> Option<String> {
        let run = self.run.as_ref().filter(|run| run.is_active())?;
        if run.phase == RunPhase::Cancelling {
            if let Some(job) = &run.job {
                job.terminate_blocking(KILL_GRACE, KILL_GRACE + QUIT_WAIT);
            }
            return None;
        }
        Some(format!(
            "{} is still running in the background; start dvcat in this project again to follow it.",
            run.command_line()
        ))
    }

    /// Pick up a run that an earlier session left behind: follow it if it is still going, or show
    /// how it ended if it finished while the interface was closed. Called once, at startup.
    pub fn reattach(&mut self, tx: &Sender<Event>) {
        if self.run.is_some() {
            return;
        }
        match runner::attach(&self.project.root, tx.clone()) {
            None => {}
            Some(Attached::Running { record, job }) => {
                let mut run = Run::restored(&record);
                run.job = Some(job);
                self.notice = Some(format!(
                    "following a run started earlier: {}",
                    run.command_line()
                ));
                self.run = Some(run);
            }
            Some(Attached::Finished {
                record,
                log,
                outcome,
                duration,
            }) => {
                let mut run = Run::restored(&record);
                run.log.push_str(&log);
                run.fixed_duration = duration;
                self.notice = Some(format!(
                    "a run finished while dvcat was closed: {}",
                    run.command_line()
                ));
                self.run = Some(run);
                self.dispatch(Event::RunFinished(outcome), tx);
            }
        }
    }

    fn move_selection_down(&mut self) {
        if self.selected + 1 < self.stage_count() {
            self.selected += 1;
            self.file_cursor = 0;
        }
    }

    fn move_selection_up(&mut self) {
        // `saturating_sub` stops at 0 instead of underflowing (usize can't be negative).
        let before = self.selected;
        self.selected = self.selected.saturating_sub(1);
        if self.selected != before {
            self.file_cursor = 0;
        }
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
    #[cfg(target_os = "linux")]
    fn a_run_blocked_by_an_outside_dvc_offers_to_stop_it() {
        let mut h = harness("echo \"ERROR: x is busy, it is being blocked by:\"; exit 255");
        // A live process that looks like `dvc`, recorded in DVC's lock file.
        let other = h.dir.path().join("dvc");
        fs::write(&other, "#!/bin/sh\nsleep 60 &\nwait\n").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = std::process::Command::new(&other)
            .arg("repro")
            .spawn()
            .unwrap();
        fs::create_dir_all(h.dir.path().join(".dvc/tmp")).unwrap();
        let lock = format!(
            r#"{{"read": {{}}, "write": {{"out": {{"pid": {}, "cmd": "dvc repro pdf2md"}}}}}}"#,
            child.id()
        );
        fs::write(h.dir.path().join(".dvc/tmp/rwlock"), lock).unwrap();

        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        let confirm = h.app.confirm.clone().expect("a question is asked");
        assert_eq!(confirm.kill, [child.id()]);
        assert!(confirm.lines.join("\n").contains("dvc repro pdf2md"));

        // Declining leaves the other process alone.
        h.press(KeyCode::Char('n'));
        assert!(h.app.confirm.is_none());
        assert!(child.try_wait().unwrap().is_none());

        // Asking again and agreeing stops it.
        h.app.confirm = Some(blocked_confirm(&[external::ExternalRun {
            pid: child.id(),
            command: "dvc repro".into(),
            writes: Vec::new(),
            reads: Vec::new(),
        }]));
        h.press(KeyCode::Char('y'));
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the other dvc is still running");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// A live process that looks like `dvc repro one`, recorded in DVC's lock as writing `out.csv`.
    #[cfg(target_os = "linux")]
    fn outside_dvc(h: &Harness) -> std::process::Child {
        let other = h.dir.path().join("dvc");
        fs::write(&other, "#!/bin/sh\nsleep 60 &\nwait\n").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o755)).unwrap();
        let child = std::process::Command::new(&other)
            .args(["repro", "a"])
            .spawn()
            .unwrap();
        fs::create_dir_all(h.dir.path().join(".dvc/tmp")).unwrap();
        let lock = format!(
            r#"{{"read": {{}}, "write": {{"{}/a.txt": {{"pid": {}, "cmd": "dvc repro a"}}}}}}"#,
            h.dir.path().display(),
            child.id()
        );
        fs::write(h.dir.path().join(".dvc/tmp/rwlock"), lock).unwrap();
        child
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_outside_run_is_noticed_without_any_failure_and_marks_its_stage() {
        let mut h = harness("exit 0");
        assert!(h.app.external.is_empty());
        let mut child = outside_dvc(&h);
        h.app.tick();
        assert_eq!(h.app.external.len(), 1);
        let marked: Vec<bool> = (0..h.app.stage_count())
            .map(|i| h.app.external_on(i).is_some())
            .collect();
        assert_eq!(marked, [true, false], "only stage a is being worked on");
        assert!(h.app.confirm.is_none(), "noticing must not interrupt");

        // Capital X asks; n leaves it be; the process ends when we agree.
        h.press(KeyCode::Char('X'));
        assert_eq!(h.app.confirm.as_ref().unwrap().kill, [child.id()]);
        h.press(KeyCode::Char('n'));
        assert!(child.try_wait().unwrap().is_none());
        h.press(KeyCode::Char('X'));
        h.press(KeyCode::Char('y'));
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "still running");
            thread::sleep(Duration::from_millis(20));
        }
        // Once it is gone the next look finds nothing.
        h.app.tick_now();
        assert!(h.app.external.is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_stage_that_would_clash_with_an_outside_run_is_refused_with_the_reason() {
        let mut h = harness("exit 0");
        let mut child = outside_dvc(&h); // writes a.txt, which stage b depends on
        h.press(KeyCode::Char('r')); // stage a writes it too
        assert!(!h.app.is_running());
        let notice = h.app.notice.clone().unwrap();
        assert!(notice.contains("cannot run 'a'"), "{notice}");
        assert!(notice.contains("it writes a.txt"), "{notice}");
        assert!(notice.contains("`dvc repro a`"), "{notice}");

        h.press(KeyCode::Char('j'));
        h.press(KeyCode::Char('r')); // stage b reads it
        let notice = h.app.notice.clone().unwrap();
        assert!(
            notice.contains("cannot run 'b': it reads a.txt"),
            "{notice}"
        );

        // With the other process gone, the run goes ahead.
        let _ = child.kill();
        let _ = child.wait();
        h.press(KeyCode::Char('r'));
        assert!(h.app.is_running() || h.app.run.is_some());
        h.pump_until_finished();
    }

    #[test]
    fn x_with_no_outside_run_says_so() {
        let mut h = harness("exit 0");
        h.press(KeyCode::Char('X'));
        assert!(h.app.confirm.is_none());
        assert!(h.app.notice.as_deref().unwrap().contains("no dvc process"));
    }

    #[test]
    fn an_ordinary_failure_asks_nothing() {
        let mut h = harness("echo went wrong; exit 2");
        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        assert!(h.app.confirm.is_none());
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
        let yaml = "stages:\n  each:\n    cmd: echo ${missing}\n";
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", yaml)]);
        let effects = app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        assert!(effects.is_empty());
        assert!(app.notice.as_deref().unwrap().contains("${missing}"));
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

    #[cfg(target_os = "linux")]
    fn grandchild_of(h: &Harness) -> u32 {
        let pid_file = h.dir.path().join("grandchild.pid");
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(text) = fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "grandchild never started");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    fn is_gone(pid: u32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|r| r.trim_start().starts_with('Z')),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn quitting_leaves_a_running_job_running_and_says_so() {
        let mut h = harness("sleep 60 &\necho $! > grandchild.pid\nwait");
        h.press(KeyCode::Char('r'));
        let grandchild = grandchild_of(&h);

        let message = h.app.shutdown().expect("a message about the run");
        assert!(message.contains("dvc repro --single-item a"), "{message}");
        assert!(message.contains("still running"), "{message}");
        thread::sleep(Duration::from_millis(300));
        assert!(!is_gone(grandchild), "the run must outlive the interface");

        h.cancel_and_wait(); // clean up
        let deadline = Instant::now() + Duration::from_secs(2);
        while !is_gone(grandchild) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(is_gone(grandchild));
    }

    #[test]
    fn quitting_with_nothing_running_has_nothing_to_say() {
        let mut h = harness("true");
        assert_eq!(h.app.shutdown(), None);
        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        assert_eq!(
            h.app.shutdown(),
            None,
            "a finished run is not a running one"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn quitting_while_cancelling_waits_until_the_process_is_gone() {
        let mut h = harness("sleep 60 &\necho $! > grandchild.pid\nwait");
        h.press(KeyCode::Char('r'));
        let grandchild = grandchild_of(&h);
        h.press(KeyCode::Char('x'));
        assert_eq!(h.app.run.as_ref().unwrap().phase, RunPhase::Cancelling);
        assert_eq!(h.app.shutdown(), None);
        assert!(
            is_gone(grandchild),
            "the cancel must be completed before exiting"
        );
    }

    #[test]
    fn a_new_session_follows_a_run_the_previous_one_left_running() {
        // The run waits for a gate file, so it is certainly still going when the next session looks.
        let mut h = harness("echo started\nwhile [ ! -f go ]; do sleep 0.02; done\necho finished");
        h.press(KeyCode::Char('r'));
        h.app.shutdown(); // "quit"; the run continues
        let old_tx = h.tx.clone();
        drop(old_tx);

        let mut next = crate::test_support::app_at(h.dir.path());
        next.dvc_lookup = |root| Ok(root.join("fake-dvc"));
        let (tx, rx) = mpsc::channel();
        next.reattach(&tx);
        let run = next.run.as_ref().expect("the run is picked up");
        assert!(run.is_active());
        assert_eq!(run.stage, "a");
        assert!(
            next.notice
                .as_deref()
                .unwrap()
                .contains("following a run started earlier")
        );
        thread::sleep(Duration::from_millis(300));
        fs::write(h.dir.path().join("go"), "").unwrap();

        let deadline = Instant::now() + WAIT;
        while next.is_running() {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            next.dispatch(event, &tx);
        }
        let run = next.run.as_ref().unwrap();
        assert_eq!(run.phase, RunPhase::Finished(RunOutcome::Succeeded));
        let log: Vec<&str> = run.log.lines().collect();
        assert!(
            log.contains(&"started") && log.contains(&"finished"),
            "{log:?}"
        );
        assert!(
            next.last_runs.contains_key("a"),
            "recorded like any other run"
        );
        assert!(
            next.run.as_ref().unwrap().duration() >= Duration::from_millis(250),
            "the time counts from the real start"
        );
    }

    #[test]
    fn a_run_that_finished_while_closed_is_shown_once_and_recorded() {
        let mut h = harness("echo done quietly");
        h.press(KeyCode::Char('r'));
        let exit_file = h.dir.path().join(".dvcat").join("dvcat-run.exit");
        let deadline = Instant::now() + WAIT;
        while !exit_file.exists() {
            assert!(Instant::now() < deadline, "the run never finished");
            thread::sleep(Duration::from_millis(20));
        }
        h.app.shutdown();

        let mut next = crate::test_support::app_at(h.dir.path());
        let (tx, _rx) = mpsc::channel();
        next.reattach(&tx);
        let run = next.run.as_ref().unwrap();
        assert_eq!(run.phase, RunPhase::Finished(RunOutcome::Succeeded));
        assert!(run.log.lines().any(|l| l == "done quietly"));
        assert!(
            next.notice
                .as_deref()
                .unwrap()
                .contains("finished while dvcat was closed")
        );
        assert_eq!(next.last_runs["a"].outcome, RunOutcome::Succeeded);

        let mut third = crate::test_support::app_at(h.dir.path());
        third.reattach(&tx);
        assert!(third.run.is_none(), "reported only once");
    }

    #[test]
    fn finishing_a_run_normally_removes_its_files() {
        let mut h = harness("echo x");
        h.press(KeyCode::Char('r'));
        h.pump_until_finished();
        let mut next = crate::test_support::app_at(h.dir.path());
        let (tx, _rx) = mpsc::channel();
        next.reattach(&tx);
        assert!(next.run.is_none());
    }

    #[test]
    fn reattaching_does_nothing_while_a_run_is_already_shown() {
        let mut h = harness("true");
        h.app.run = Some(Run::new("a".into(), false));
        let (tx, _rx) = mpsc::channel();
        h.app.reattach(&tx);
        assert_eq!(h.app.run.as_ref().unwrap().stage, "a");
        assert!(h.app.notice.is_none());
    }
}

#[cfg(test)]
mod experiments_view_tests {
    use std::{fs, os::unix::fs::PermissionsExt, sync::mpsc, time::Instant};

    use tempfile::TempDir;

    use super::*;
    use crate::{
        experiments::{Column, Direction},
        test_support::app_with_files,
    };

    const ONE_STAGE: &str = "stages:\n  a:\n    cmd: x\n";
    const WAIT: Duration = Duration::from_secs(10);

    struct Harness {
        dir: TempDir,
        app: App,
        tx: Sender<Event>,
        rx: mpsc::Receiver<Event>,
    }

    /// A fake `dvc` that logs each invocation to `calls.log` and then runs `body`.
    fn harness(body: &str) -> Harness {
        let (dir, mut app) = app_with_files(&[("dvc.yaml", ONE_STAGE)]);
        let script = format!("#!/bin/sh\necho \"$*\" >> calls.log\n{body}\n");
        let path = dir.path().join("fake-dvc");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        app.dvc_lookup = |root| Ok(root.join("fake-dvc"));
        let (tx, rx) = mpsc::channel();
        Harness { dir, app, tx, rx }
    }

    fn serving_two() -> Harness {
        let json = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        harness(&format!("cat '{}'", json.display()))
    }

    impl Harness {
        fn press(&mut self, code: KeyCode) -> Vec<Effect> {
            let effects = self
                .app
                .update(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
            for effect in effects.clone() {
                self.app.execute(effect, &self.tx);
            }
            effects
        }

        fn wait_for_data(&mut self) {
            let deadline = Instant::now() + WAIT;
            while self.app.experiments.loading {
                let left = deadline.saturating_duration_since(Instant::now());
                let event = self.rx.recv_timeout(left).expect("no experiments event");
                self.app.dispatch(event, &self.tx);
            }
        }

        fn fetch_count(&self) -> usize {
            fs::read_to_string(self.dir.path().join("calls.log"))
                .map(|log| log.lines().filter(|l| *l == "exp show --json -A").count())
                .unwrap_or(0)
        }
    }

    #[test]
    fn e_opens_the_view_and_starts_one_fetch() {
        let mut h = serving_two();
        let effects = h.press(KeyCode::Char('e'));
        assert_eq!(effects, [Effect::FetchExperiments]);
        assert_eq!(h.app.view, View::Experiments);
        assert!(h.app.experiments.loading);

        h.wait_for_data();
        assert!(!h.app.experiments.loading);
        assert_eq!(h.app.experiments.table().unwrap().rows.len(), 4);
        assert_eq!(h.fetch_count(), 1);
    }

    #[test]
    fn leaving_and_returning_does_not_fetch_again_but_f5_does() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();

        h.press(KeyCode::Char('e')); // back to the pipeline
        assert_eq!(h.app.view, View::Pipeline);
        assert!(
            h.press(KeyCode::Char('e')).is_empty(),
            "data is already there"
        );
        assert_eq!(h.fetch_count(), 1);

        assert_eq!(h.press(KeyCode::F(5)), [Effect::FetchExperiments]);
        h.wait_for_data();
        assert_eq!(h.fetch_count(), 2);
    }

    #[test]
    fn f5_while_a_fetch_is_in_flight_does_not_start_another() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        assert!(h.press(KeyCode::F(5)).is_empty());
        h.wait_for_data();
        assert_eq!(h.fetch_count(), 1);
    }

    #[test]
    fn nothing_is_fetched_while_a_stage_is_running_and_the_user_is_told_why() {
        let mut h = serving_two();
        h.app.run = Some(Run::new("a".into(), false)); // an active run
        assert!(h.press(KeyCode::Char('e')).is_empty());
        assert_eq!(h.app.view, View::Experiments, "the view still opens");
        assert!(!h.app.experiments.loading);
        assert!(
            h.app
                .notice
                .as_deref()
                .unwrap()
                .contains("holds the repository lock")
        );

        h.press(KeyCode::F(5));
        assert!(h.app.notice.is_some());
        assert_eq!(h.fetch_count(), 0);

        // Once the run is over, F5 works.
        h.app.run.as_mut().unwrap().phase = RunPhase::Finished(RunOutcome::Succeeded);
        assert_eq!(h.press(KeyCode::F(5)), [Effect::FetchExperiments]);
        h.wait_for_data();
        assert!(h.app.experiments.table().is_some());
    }

    #[test]
    fn a_failing_command_becomes_an_error_in_the_view_and_f5_retries() {
        let mut h = harness("echo 'ERROR: not a git repository' >&2; exit 255");
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        let Some(Err(message)) = &h.app.experiments.data else {
            panic!("expected an error, got {:?}", h.app.experiments.data);
        };
        assert!(
            message.contains("not a git repository") && message.contains("255"),
            "{message}"
        );
        assert_eq!(
            h.press(KeyCode::F(5)),
            [Effect::FetchExperiments],
            "retry is possible"
        );
        h.wait_for_data();
    }

    #[test]
    fn missing_dvc_is_reported_in_the_view_with_the_places_searched() {
        let mut h = serving_two();
        h.app.dvc_lookup = |_| {
            Err(DvcNotFound {
                searched: vec!["somewhere".into()],
            })
        };
        let effects = h.app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::NONE,
        )));
        for effect in effects {
            h.app.execute(effect, &h.tx);
        }
        let Some(Err(message)) = &h.app.experiments.data else {
            panic!("expected an error");
        };
        assert!(message.contains("somewhere"), "{message}");
        assert!(!h.app.experiments.loading);
    }

    #[test]
    fn cursor_keys_stay_inside_the_table() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        for _ in 0..20 {
            h.press(KeyCode::Char('j'));
            h.press(KeyCode::Char('l'));
        }
        assert_eq!(h.app.experiments.row, 3, "4 rows");
        assert_eq!(h.app.experiments.column, 5, "name + 3 params + 2 metrics");
        for _ in 0..20 {
            h.press(KeyCode::Char('k'));
            h.press(KeyCode::Char('h'));
        }
        assert_eq!((h.app.experiments.row, h.app.experiments.column), (0, 0));
    }

    #[test]
    fn s_sorts_by_the_cursor_column_and_pressing_again_flips_the_direction() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        // Columns: name, features.scale, train.epochs, train.lr, auc, loss.
        for _ in 0..4 {
            h.press(KeyCode::Right);
        }
        let auc = Column::Metric("metrics.json:auc".into());
        h.press(KeyCode::Char('s'));
        assert_eq!(
            h.app.experiments.sort.direction_of(&auc),
            Some(Direction::Ascending)
        );
        h.press(KeyCode::Char('s'));
        assert_eq!(
            h.app.experiments.sort.direction_of(&auc),
            Some(Direction::Descending)
        );
    }

    #[test]
    fn run_keys_do_nothing_in_the_experiments_view_but_x_still_cancels() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        assert!(h.press(KeyCode::Char('r')).is_empty());
        assert!(h.press(KeyCode::Char('R')).is_empty());
        assert!(
            h.app.run.is_none(),
            "r must not start a stage run from here"
        );

        h.press(KeyCode::Char('x'));
        assert_eq!(h.app.notice.as_deref(), Some("nothing is running"));

        h.app.run = Some(Run::new("a".into(), false));
        let effects = h.app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        assert_eq!(effects, [Effect::CancelRun]);
        assert_eq!(h.app.run.as_ref().unwrap().phase, RunPhase::Cancelling);
    }

    #[test]
    fn esc_returns_to_the_pipeline_and_q_still_quits() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        h.press(KeyCode::Esc);
        assert_eq!(h.app.view, View::Pipeline);
        h.press(KeyCode::Char('e'));
        h.press(KeyCode::Char('q'));
        assert!(h.app.should_quit);
    }

    #[test]
    fn a_smaller_table_pulls_the_cursor_back_inside() {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        h.app.experiments.row = 3;
        h.app.experiments.column = 5;
        let small = crate::experiments::parse(
            r#"[{"rev":"workspace","name":null,"error":null,"experiments":null,"data":{"timestamp":null,"params":{},"metrics":{}}}]"#,
        )
        .unwrap();
        h.app.update(Event::Experiments(Ok(small)));
        assert_eq!((h.app.experiments.row, h.app.experiments.column), (0, 0));
    }

    #[test]
    fn cursor_keys_before_the_data_arrives_do_not_panic() {
        let mut h = serving_two();
        h.app.view = View::Experiments;
        for code in [KeyCode::Char('j'), KeyCode::Char('l'), KeyCode::Char('s')] {
            h.press(code);
        }
        assert_eq!((h.app.experiments.row, h.app.experiments.column), (0, 0));
    }

    // ----- the "new experiment" form -----

    fn type_text(h: &mut Harness, text: &str) {
        for c in text.chars() {
            h.press(KeyCode::Char(c));
        }
    }

    fn with_table_loaded() -> Harness {
        let mut h = serving_two();
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        h
    }

    #[test]
    fn v_lists_params_and_space_hides_a_whole_group() {
        let mut h = with_table_loaded();
        let all = h
            .app
            .experiments
            .columns(h.app.experiments.table().unwrap())
            .len();
        h.press(KeyCode::Char('v'));
        assert!(h.app.param_picker.is_some());
        // The picker is modal: q closes it instead of quitting.
        h.press(KeyCode::Char(' ')); // first entry: the whole first file
        let now = h
            .app
            .experiments
            .columns(h.app.experiments.table().unwrap())
            .len();
        assert!(now < all, "{now} < {all}");
        assert!(!h.app.experiments.hidden.is_empty());
        h.press(KeyCode::Char('q'));
        assert!(h.app.param_picker.is_none());
        assert!(!h.app.should_quit);
        // Space again (reopen) brings them back; `a` shows everything at once.
        h.press(KeyCode::Char('v'));
        h.press(KeyCode::Char('a'));
        assert!(h.app.experiments.hidden.is_empty());
    }

    #[test]
    fn capital_h_hides_the_param_under_the_cursor_only() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('H')); // on the name column
        assert!(h.app.experiments.hidden.is_empty());
        let table = h.app.experiments.table().unwrap();
        let position = h
            .app
            .experiments
            .columns(table)
            .iter()
            .position(|c| matches!(c, experiments::Column::Param(_)))
            .unwrap();
        h.app.experiments.column = position;
        h.press(KeyCode::Char('H'));
        assert_eq!(h.app.experiments.hidden.len(), 1);
    }

    #[test]
    fn the_cursor_stays_inside_the_table_when_columns_disappear() {
        let mut h = with_table_loaded();
        h.app.experiments.column = 99;
        h.press(KeyCode::Char('v'));
        h.press(KeyCode::Char(' '));
        let columns = h
            .app
            .experiments
            .columns(h.app.experiments.table().unwrap())
            .len();
        assert!(h.app.experiments.column < columns);
    }

    fn calls(h: &Harness) -> Vec<String> {
        fs::read_to_string(h.dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn n_opens_a_form_filled_from_the_row_under_the_cursor() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('j')); // master (the baseline)
        h.press(KeyCode::Char('n'));
        let form = h.app.form.as_ref().expect("form open");
        assert_eq!(form.base, "master");
        assert!(
            form.fields
                .iter()
                .any(|f| f.label == "params.yaml:train.lr" && f.value == "0.01")
        );
    }

    #[test]
    fn n_needs_loaded_experiments_and_no_active_run() {
        let mut h = serving_two();
        h.app.view = View::Experiments;
        h.press(KeyCode::Char('n'));
        assert!(h.app.form.is_none());
        assert!(h.app.notice.as_deref().unwrap().contains("F5"));

        let mut h = with_table_loaded();
        h.app.run = Some(Run::new("a".into(), false));
        h.press(KeyCode::Char('n'));
        assert!(h.app.form.is_none());
        assert!(
            h.app
                .notice
                .as_deref()
                .unwrap()
                .contains("already in progress")
        );
    }

    #[test]
    fn the_form_is_modal_and_esc_closes_it() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('n'));
        type_text(&mut h, "qe"); // would quit / leave the view
        assert!(!h.app.should_quit);
        assert_eq!(h.app.view, View::Experiments);
        assert_eq!(h.app.form.as_ref().unwrap().name, "qe");
        h.press(KeyCode::Esc);
        assert!(h.app.form.is_none());
        assert_eq!(h.app.view, View::Experiments);
    }

    #[test]
    fn submitting_runs_dvc_exp_run_shows_the_log_and_reloads_the_table() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('j')); // master
        h.press(KeyCode::Char('n'));
        type_text(&mut h, "my-exp");
        let lr = h
            .app
            .form
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .position(|f| f.label == "params.yaml:train.lr")
            .unwrap();
        for _ in 0..=lr {
            h.press(KeyCode::Tab);
        }
        for _ in 0..4 {
            h.press(KeyCode::Backspace); // "0.01"
        }
        type_text(&mut h, "0.2");
        let effects = h.press(KeyCode::Enter);

        assert_eq!(effects, [Effect::StartExperiment]);
        assert!(h.app.form.is_none());
        assert_eq!(h.app.view, View::Pipeline, "the log pane lives there");
        let run = h.app.run.as_ref().unwrap();
        assert!(run.kind == RunKind::Experiment && run.is_active());
        assert_eq!(
            run.command_line(),
            "dvc exp run --temp -n my-exp -S params.yaml:train.lr=0.2"
        );

        // Let the fake process finish; the app then asks for fresh experiments.
        let deadline = Instant::now() + WAIT;
        while h.app.is_running() {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = h.rx.recv_timeout(left).expect("run never finished");
            h.app.dispatch(event, &h.tx);
        }
        assert!(h.app.experiments.loading, "a refetch was requested");
        h.wait_for_data();
        assert!(
            calls(&h).contains(&"exp run --temp -n my-exp -S params.yaml:train.lr=0.2".to_owned())
        );
        assert_eq!(h.fetch_count(), 2);
    }

    #[test]
    fn a_failed_experiment_is_reported_and_still_reloads_the_table() {
        let mut h = harness("echo boom >&2; exit 3");
        // Seed the table from the sample file so the form can open.
        let json = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        let table = experiments::parse(&fs::read_to_string(json).unwrap()).unwrap();
        h.app.experiments.data = Some(Ok(table));
        h.app.view = View::Experiments;
        h.press(KeyCode::Char('n'));
        h.press(KeyCode::Enter);
        let deadline = Instant::now() + WAIT;
        while h.app.is_running() {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = h.rx.recv_timeout(left).expect("run never finished");
            h.app.dispatch(event, &h.tx);
        }
        let run = h.app.run.as_ref().unwrap();
        assert_eq!(
            run.phase,
            RunPhase::Finished(RunOutcome::Failed { code: Some(3) })
        );
        assert!(run.log.lines().any(|l| l.contains("boom")));
        assert!(h.app.experiments.loading);
    }

    #[test]
    fn invalid_input_keeps_the_form_open_with_a_message() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('n'));
        type_text(&mut h, "bad name");
        let effects = h.press(KeyCode::Enter);
        assert!(effects.is_empty());
        assert!(h.app.form.as_ref().unwrap().error.is_some());
        assert!(h.app.run.is_none());
    }

    // ----- apply / remove -----

    /// Move the cursor onto the row with this label.
    fn cursor_on(h: &mut Harness, label: &str) {
        let table = h.app.experiments.table().unwrap();
        let rows = table.ordered(&h.app.experiments.sort);
        h.app.experiments.row = rows.iter().position(|r| r.label == label).unwrap();
    }

    fn wait_until_finished(h: &mut Harness) {
        let deadline = Instant::now() + WAIT;
        while h.app.is_running() {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = h.rx.recv_timeout(left).expect("run never finished");
            h.app.dispatch(event, &h.tx);
        }
    }

    #[test]
    fn a_and_d_only_work_on_experiment_rows() {
        let mut h = with_table_loaded();
        for label in ["workspace", "master"] {
            cursor_on(&mut h, label);
            h.press(KeyCode::Char('a'));
            h.press(KeyCode::Char('d'));
            assert!(h.app.confirm.is_none(), "{label}");
            assert!(h.app.notice.as_deref().unwrap().contains("experiment row"));
        }
    }

    #[test]
    fn a_asks_first_and_n_or_esc_changes_nothing() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-010");
        h.press(KeyCode::Char('a'));
        let confirm = h.app.confirm.as_ref().expect("asks first");
        assert_eq!(confirm.title, "Apply lr-010?");
        assert_eq!(confirm.args, ["exp", "apply", "lr-010"]);
        assert!(h.app.run.is_none(), "nothing runs before the answer");

        h.press(KeyCode::Char('n'));
        assert!(h.app.confirm.is_none() && h.app.run.is_none());
        h.press(KeyCode::Char('d'));
        h.press(KeyCode::Esc);
        assert!(h.app.confirm.is_none() && h.app.run.is_none());
        assert_eq!(calls(&h), ["exp show --json -A"], "only the initial fetch");
    }

    #[test]
    fn the_question_is_modal() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('d'));
        for code in [KeyCode::Char('e'), KeyCode::Char('x'), KeyCode::Char('j')] {
            h.press(code);
        }
        assert!(h.app.confirm.is_some());
        assert_eq!(h.app.view, View::Experiments);
        assert!(!h.app.should_quit);
    }

    #[test]
    fn y_applies_shows_the_log_and_reloads_the_table() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-010");
        h.press(KeyCode::Char('a'));
        let effects = h.press(KeyCode::Char('y'));
        assert_eq!(effects, [Effect::StartExperiment]);
        assert_eq!(h.app.view, View::Pipeline);
        assert_eq!(
            h.app.run.as_ref().unwrap().command_line(),
            "dvc exp apply lr-010"
        );
        wait_until_finished(&mut h);
        assert!(h.app.experiments.loading, "table reloads afterwards");
        h.wait_for_data();
        assert!(calls(&h).contains(&"exp apply lr-010".to_owned()));
        assert_eq!(h.fetch_count(), 2);
    }

    /// A fake `dvc` that knows these remotes (`name<TAB>url` lines) and the default one.
    fn with_remotes(list: &str, default: &str) -> Harness {
        let mut h = harness(&format!(
            "case \"$1 $2\" in\n\
             \"remote list\") printf '{list}' ;;\n\
             \"remote default\") echo '{default}' ;;\n\
             *) echo \"ran $*\" ;;\nesac"
        ));
        h.app.view = View::Pipeline;
        h
    }

    /// Deliver the background `dvc remote` answer.
    fn wait_for_remotes(h: &mut Harness) {
        let event = h.rx.recv_timeout(WAIT).expect("no remotes event");
        h.app.dispatch(event, &h.tx);
    }

    #[test]
    fn a_single_remote_is_asked_about_directly_and_then_runs() {
        for (key, title, command) in [
            ('P', "Pull from origin?", "pull"),
            ('U', "Push to origin?", "push"),
        ] {
            let mut h = with_remotes("origin\ts3://bucket\\n", "origin");
            let effects = h.press(KeyCode::Char(key));
            let transfer = if key == 'P' {
                Transfer::Pull
            } else {
                Transfer::Push
            };
            assert_eq!(effects, [Effect::LoadRemotes(transfer)]);
            wait_for_remotes(&mut h);
            assert!(h.app.remote_picker.is_none());
            let confirm = h.app.confirm.as_ref().expect("asks first");
            assert_eq!(confirm.title, title);
            assert!(confirm.lines[0].contains("s3://bucket"));
            assert!(h.app.run.is_none(), "nothing runs before the answer");

            let effects = h.press(KeyCode::Char('y'));
            assert_eq!(effects, [Effect::StartExperiment]);
            let run = h.app.run.as_ref().unwrap();
            assert_eq!(run.kind, RunKind::Remote);
            assert_eq!(run.command_line(), format!("dvc {command} -r origin"));
            wait_until_finished(&mut h);
            assert!(calls(&h).contains(&format!("{command} -r origin")));
        }
    }

    #[test]
    fn t_and_c_add_the_selected_stage_and_the_run_cache_to_the_command() {
        let mut h = with_remotes("origin\tx\\n", "origin");
        let stage = h.app.selected_stage().unwrap().name.clone();
        h.press(KeyCode::Char('U'));
        wait_for_remotes(&mut h);
        h.press(KeyCode::Char('t'));
        h.press(KeyCode::Char('c'));
        assert!(h.app.confirm.as_ref().unwrap().toggles.iter().all(|t| t.on));
        h.press(KeyCode::Char('c')); // off again
        h.press(KeyCode::Char('c')); // and on
        h.press(KeyCode::Enter);
        wait_until_finished(&mut h);
        let expected = format!("push -r origin {stage} --run-cache");
        assert!(calls(&h).contains(&expected), "{:?}", calls(&h));
        assert_eq!(
            h.app.run.as_ref().unwrap().command_line(),
            format!("dvc {expected}")
        );
    }

    #[test]
    fn the_stage_is_not_offered_outside_the_stage_views() {
        let mut h = with_remotes("origin\tx\\n", "origin");
        h.app.view = View::Plots;
        h.press(KeyCode::Char('P'));
        wait_for_remotes(&mut h);
        let toggles = &h.app.confirm.as_ref().unwrap().toggles;
        assert_eq!(toggles.len(), 1);
        assert_eq!(toggles[0].key, 'c');
    }

    #[test]
    fn several_remotes_are_listed_and_the_chosen_one_is_used() {
        let mut h = with_remotes("origin\ts3://a\\nbackup\t/mnt/b\\n", "backup");
        h.press(KeyCode::Char('U'));
        wait_for_remotes(&mut h);
        let picker = h.app.remote_picker.as_ref().expect("a list to choose from");
        assert_eq!(picker.cursor, 1, "starts on the default remote");
        assert!(h.app.confirm.is_none());

        h.press(KeyCode::Char('k'));
        h.press(KeyCode::Enter);
        assert!(h.app.remote_picker.is_none());
        assert_eq!(h.app.confirm.as_ref().unwrap().title, "Push to origin?");
        h.press(KeyCode::Char('y'));
        wait_until_finished(&mut h);
        assert!(calls(&h).contains(&"push -r origin".to_owned()));
    }

    #[test]
    fn no_remote_is_reported_and_declining_runs_nothing() {
        let mut h = with_remotes("", "");
        h.press(KeyCode::Char('P'));
        wait_for_remotes(&mut h);
        assert!(h.app.confirm.is_none() && h.app.remote_picker.is_none());
        assert!(h.app.notice.as_ref().unwrap().contains("no remote"));

        let mut h = with_remotes("a\tx\\nb\ty\\n", "");
        h.press(KeyCode::Char('U'));
        wait_for_remotes(&mut h);
        h.press(KeyCode::Esc);
        assert!(h.app.remote_picker.is_none() && h.app.run.is_none());
        assert!(!calls(&h).iter().any(|c| c.starts_with("push")));
    }

    #[test]
    fn pull_and_push_are_refused_while_something_runs() {
        let mut h = with_remotes("origin\tx\\n", "origin");
        h.app.run = Some(Run::experiment(
            "x".into(),
            vec!["exp".into(), "run".into()],
        ));
        assert!(h.press(KeyCode::Char('P')).is_empty());
        assert!(
            h.app
                .notice
                .as_ref()
                .unwrap()
                .contains("already in progress")
        );
    }

    #[test]
    fn a_remote_run_survives_being_recorded_and_restored() {
        let run = Run::remote(
            "dvc pull -r origin".to_owned(),
            remotes::transfer_args(Transfer::Pull, "origin"),
        );
        assert_eq!(Run::restored(&run.record()).kind, RunKind::Remote);
    }

    #[test]
    fn enter_confirms_removal_of_the_chosen_experiment() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('d'));
        assert_eq!(h.app.confirm.as_ref().unwrap().title, "Remove lr-005?");
        h.press(KeyCode::Enter);
        wait_until_finished(&mut h);
        assert!(calls(&h).contains(&"exp remove lr-005".to_owned()));
    }

    #[test]
    fn a_and_d_are_refused_while_something_runs() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-010");
        h.app.run = Some(Run::new("a".into(), false));
        h.press(KeyCode::Char('a'));
        assert!(h.app.confirm.is_none());
        assert!(
            h.app
                .notice
                .as_deref()
                .unwrap()
                .contains("already in progress")
        );
    }

    // ----- experiment messages -----

    fn give_message(h: &mut Harness, label: &str, message: &str) {
        let Some(Ok(table)) = &mut h.app.experiments.data else {
            panic!("no table");
        };
        let rev = table
            .rows
            .iter()
            .find(|r| r.label == label)
            .unwrap()
            .rev
            .clone();
        table.messages.insert(rev, message.to_owned());
    }

    #[test]
    fn c_edits_the_first_line_of_the_message_only_on_experiment_rows() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "master");
        h.press(KeyCode::Char('c'));
        assert!(h.app.prompt.is_none());
        assert!(h.app.notice.as_deref().unwrap().contains("experiment row"));

        give_message(&mut h, "lr-005", "try a bigger step\n\nlonger body");
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('c'));
        assert_eq!(h.app.prompt.as_ref().unwrap().value, "try a bigger step");

        for _ in 0.."try a bigger step".len() {
            h.press(KeyCode::Backspace);
        }
        for c in "new".chars() {
            h.press(KeyCode::Char(c));
        }
        let effects = h.app.update(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(h.app.prompt.is_none());
        let [Effect::SaveMessage { rev, message }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert!(rev.starts_with("58d3bd4"));
        assert_eq!(message, "new\n\nlonger body");
    }

    #[test]
    fn a_message_without_a_text_starts_with_an_empty_field() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('c'));
        assert_eq!(h.app.prompt.as_ref().unwrap().value, "");
    }

    // ----- exp branch -----

    #[test]
    fn b_opens_a_prompt_with_the_default_name_only_on_experiment_rows() {
        let mut h = with_table_loaded();
        for label in ["workspace", "master"] {
            cursor_on(&mut h, label);
            h.press(KeyCode::Char('b'));
            assert!(h.app.prompt.is_none(), "{label}");
            assert!(h.app.notice.as_deref().unwrap().contains("experiment row"));
        }
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('b'));
        assert_eq!(h.app.prompt.as_ref().unwrap().value, "lr-005-branch");
    }

    #[test]
    fn b_is_refused_while_a_run_is_active_or_the_table_is_missing() {
        let mut h = serving_two();
        h.app.view = View::Experiments;
        h.press(KeyCode::Char('b'));
        assert!(h.app.prompt.is_none() && h.app.notice.as_deref().unwrap().contains("F5"));

        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.app.run = Some(Run::new("a".into(), false));
        h.press(KeyCode::Char('b'));
        assert!(h.app.prompt.is_none());
    }

    #[test]
    fn the_prompt_is_modal_and_esc_runs_nothing() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('b'));
        type_text(&mut h, "qe");
        assert!(!h.app.should_quit && h.app.view == View::Experiments);
        assert!(h.app.prompt.as_ref().unwrap().value.ends_with("qe"));
        h.press(KeyCode::Esc);
        assert!(h.app.prompt.is_none() && h.app.run.is_none());
        assert_eq!(calls(&h), ["exp show --json -A"]);
    }

    #[test]
    fn invalid_names_keep_the_prompt_open() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('b'));
        type_text(&mut h, " x");
        let effects = h.press(KeyCode::Enter);
        assert!(effects.is_empty() && h.app.run.is_none());
        assert!(h.app.prompt.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn enter_runs_exp_branch_and_a_success_names_the_branch_without_reloading_the_table() {
        let mut h = with_table_loaded();
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('b'));
        for _ in 0.."-branch".len() {
            h.press(KeyCode::Backspace);
        }
        type_text(&mut h, "-keep");
        let effects = h.press(KeyCode::Enter);
        assert_eq!(effects, [Effect::StartExperiment]);
        assert_eq!(h.app.view, View::Pipeline);
        assert_eq!(
            h.app.run.as_ref().unwrap().command_line(),
            "dvc exp branch lr-005 lr-005-keep"
        );
        wait_until_finished(&mut h);
        assert!(
            h.app
                .notice
                .as_deref()
                .unwrap()
                .contains("git switch lr-005-keep"),
            "{:?}",
            h.app.notice
        );
        assert!(
            !h.app.experiments.loading,
            "a branch does not change the table"
        );
        assert_eq!(h.fetch_count(), 1);
    }

    #[test]
    fn a_failed_branch_shows_the_error_and_no_success_notice() {
        let mut h = harness("echo 'fatal: branch exists' >&2; exit 1");
        let json = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        let table = experiments::parse(&fs::read_to_string(json).unwrap()).unwrap();
        h.app.experiments.data = Some(Ok(table));
        h.app.view = View::Experiments;
        cursor_on(&mut h, "lr-005");
        h.press(KeyCode::Char('b'));
        h.press(KeyCode::Enter);
        wait_until_finished(&mut h);
        let run = h.app.run.as_ref().unwrap();
        assert_eq!(
            run.phase,
            RunPhase::Finished(RunOutcome::Failed { code: Some(1) })
        );
        assert!(run.log.lines().any(|l| l.contains("branch exists")));
        assert!(
            h.app
                .notice
                .as_deref()
                .is_none_or(|n| !n.contains("created"))
        );
    }

    // ----- run durations -----

    #[test]
    fn finishing_a_stage_run_records_its_outcome_and_duration() {
        let mut h = harness("sleep 0.2");
        h.app.run = None;
        h.press(KeyCode::Char('r'));
        wait_until_finished(&mut h);
        let last = h.app.last_runs.get("a").expect("recorded");
        assert_eq!(last.outcome, RunOutcome::Succeeded);
        assert!(
            last.duration >= Duration::from_millis(150),
            "{:?}",
            last.duration
        );
        let run = h.app.run.as_ref().unwrap();
        assert_eq!(
            run.duration(),
            last.duration,
            "the log title freezes at the same value"
        );
    }

    #[test]
    fn a_running_run_reports_growing_time_and_a_new_run_replaces_the_record() {
        let mut h = harness("exit 3");
        h.press(KeyCode::Char('r'));
        let early = h.app.run.as_ref().unwrap().duration();
        wait_until_finished(&mut h);
        assert!(h.app.run.as_ref().unwrap().duration() >= early);
        assert_eq!(
            h.app.last_runs["a"].outcome,
            RunOutcome::Failed { code: Some(3) }
        );
        h.press(KeyCode::Char('r'));
        wait_until_finished(&mut h);
        assert_eq!(h.app.last_runs.len(), 1, "one record per stage");
    }

    #[test]
    fn experiment_runs_and_failures_to_start_leave_no_stage_record() {
        let mut h = harness("true");
        h.app.run = Some(Run::experiment(
            "exp x".into(),
            vec!["exp".into(), "run".into()],
        ));
        h.app.update(Event::RunFinished(RunOutcome::Succeeded));
        assert!(h.app.last_runs.is_empty());
        h.app.run = Some(Run::new("a".into(), false));
        h.app
            .update(Event::RunFinished(RunOutcome::NotStarted("no dvc".into())));
        assert!(h.app.last_runs.is_empty());
    }

    #[test]
    fn a_run_that_dvc_skipped_leaves_no_last_run() {
        let mut h = harness("echo \"Stage 'a' didn't change, skipping\"");
        h.press(KeyCode::Char('r'));
        wait_until_finished(&mut h);
        assert_eq!(
            h.app.run.as_ref().unwrap().phase,
            RunPhase::Finished(RunOutcome::Succeeded)
        );
        assert!(h.app.last_runs.is_empty(), "{:?}", h.app.last_runs);
    }

    #[test]
    fn a_skipped_run_keeps_the_previous_record_of_the_stage() {
        let mut h = harness("echo running; sleep 0.1");
        h.press(KeyCode::Char('r'));
        wait_until_finished(&mut h);
        let before = h.app.last_runs["a"].clone();
        // The next run only skips.
        let script = h.dir.path().join("fake-dvc");
        fs::write(
            &script,
            "#!/bin/sh\necho \"Stage 'a' didn't change, skipping\"\n",
        )
        .unwrap();
        h.press(KeyCode::Char('r'));
        wait_until_finished(&mut h);
        assert_eq!(h.app.last_runs["a"], before);
    }

    #[test]
    fn only_the_requested_stage_counts_not_skipped_upstream_ones() {
        let mut h = harness(
            "echo \"Stage 'up' didn't change, skipping\"; echo \"Running stage 'a':\"; sleep 0.1",
        );
        h.press(KeyCode::Char('R'));
        wait_until_finished(&mut h);
        assert!(
            h.app.last_runs.contains_key("a"),
            "a ran, even though its upstream was skipped"
        );
    }

    #[test]
    fn a_failed_run_is_recorded_even_if_the_log_mentions_other_skips() {
        let mut h = harness("echo \"Stage 'up' didn't change, skipping\"; exit 1");
        h.press(KeyCode::Char('R'));
        wait_until_finished(&mut h);
        assert_eq!(
            h.app.last_runs["a"].outcome,
            RunOutcome::Failed { code: Some(1) }
        );
    }

    // ----- run times of experiments, remembered between sessions -----

    #[test]
    fn number_keys_switch_views_directly() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('1'));
        assert_eq!(h.app.view, View::Pipeline);
        h.press(KeyCode::Char('2'));
        assert_eq!(h.app.view, View::Dag);
        h.press(KeyCode::Char('3'));
        assert_eq!(h.app.view, View::Experiments);
        h.press(KeyCode::Char('3'));
        assert_eq!(h.app.view, View::Experiments, "already there");
        h.press(KeyCode::Char('2'));
        h.press(KeyCode::Char('1'));
        assert_eq!(h.app.view, View::Pipeline);
    }

    #[test]
    fn digits_typed_into_a_form_do_not_switch_views() {
        let mut h = with_table_loaded();
        h.press(KeyCode::Char('n'));
        h.press(KeyCode::Char('2'));
        assert_eq!(h.app.view, View::Experiments);
        assert!(h.app.form.as_ref().unwrap().name.ends_with('2'));
    }

    #[test]
    fn an_experiment_run_records_its_time_under_the_name_dvc_reports() {
        let mut h = harness(
            "case \"$1\" in exp) [ \"$2\" = run ] && { echo 'Ran experiment(s): my-exp'; sleep 0.2; } ;; esac",
        );
        let json = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/exp_show/two.json");
        let table = experiments::parse(&fs::read_to_string(json).unwrap()).unwrap();
        h.app.experiments.data = Some(Ok(table));
        h.app.view = View::Experiments;
        h.press(KeyCode::Char('n'));
        for c in "my-exp".chars() {
            h.press(KeyCode::Char(c));
        }
        h.press(KeyCode::Enter);
        wait_until_finished(&mut h);
        let secs = h.app.experiment_times["my-exp"];
        assert!(secs >= 0.15, "{secs}");
    }

    #[test]
    fn apply_remove_and_branch_runs_record_no_experiment_time() {
        let mut h = harness("echo 'Ran experiment(s): ghost'");
        for args in [["exp", "apply", "x"], ["exp", "remove", "x"]] {
            h.app.run = Some(Run::experiment("x".into(), args.map(String::from).into()));
            h.app.update(Event::RunFinished(RunOutcome::Succeeded));
        }
        assert!(h.app.experiment_times.is_empty());
    }

    #[test]
    fn a_failed_experiment_run_records_no_time() {
        let mut h = harness("true");
        let mut run = Run::experiment(
            "exp x".into(),
            ["exp", "run", "--temp"].map(String::from).into(),
        );
        run.log.push_str("Ran experiment(s): half\n");
        h.app.run = Some(run);
        h.app
            .update(Event::RunFinished(RunOutcome::Failed { code: Some(1) }));
        assert!(h.app.experiment_times.is_empty());
    }

    #[test]
    fn a_loaded_table_gets_the_run_time_column_for_known_experiments_only() {
        let mut h = serving_two();
        h.app.experiment_times.insert("lr-005".into(), 12.5);
        h.press(KeyCode::Char('e'));
        h.wait_for_data();
        let table = h.app.experiments.table().unwrap();
        assert!(
            table
                .columns()
                .contains(&crate::experiments::Column::Duration)
        );
        let secs = |label: &str| {
            table
                .rows
                .iter()
                .find(|r| r.label == label)
                .unwrap()
                .run_secs
        };
        assert_eq!(secs("lr-005"), Some(12.5));
        assert_eq!(secs("lr-010"), None);
        assert_eq!(secs("master"), None);
    }

    #[test]
    fn without_any_known_time_there_is_no_run_time_column() {
        let h = with_table_loaded();
        let table = h.app.experiments.table().unwrap();
        assert!(
            !table
                .columns()
                .contains(&crate::experiments::Column::Duration)
        );
    }

    #[test]
    fn times_are_saved_on_every_finished_run_and_loaded_by_the_next_session() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".dvc")).unwrap();
        fs::write(dir.path().join("dvc.yaml"), ONE_STAGE).unwrap();
        let script = dir.path().join("fake-dvc");
        fs::write(&script, "#!/bin/sh\nsleep 0.1\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut app = crate::test_support::app_at(dir.path());
        app.dvc_lookup = |root| Ok(root.join("fake-dvc"));
        app.enable_history();
        let (tx, rx) = mpsc::channel();
        app.dispatch(
            Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            &tx,
        );
        let deadline = Instant::now() + WAIT;
        while app.is_running() {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            app.dispatch(event, &tx);
        }
        app.experiment_times.insert("e1".into(), 7.0);
        app.save_history();

        let mut next = crate::test_support::app_at(dir.path());
        assert!(next.last_runs.is_empty(), "history is opt-in");
        next.enable_history();
        assert_eq!(next.last_runs["a"].outcome, RunOutcome::Succeeded);
        assert!(next.last_runs["a"].duration >= Duration::from_millis(80));
        assert_eq!(next.experiment_times["e1"], 7.0);
    }

    #[test]
    fn nothing_is_written_unless_history_was_enabled() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".dvc")).unwrap();
        fs::write(dir.path().join("dvc.yaml"), ONE_STAGE).unwrap();
        let mut app = crate::test_support::app_at(dir.path());
        app.run = Some(Run::new("a".into(), false));
        app.update(Event::RunFinished(RunOutcome::Succeeded));
        assert!(!dir.path().join(".dvc/tmp").exists());
    }
}

#[cfg(test)]
mod files_pane_tests {
    use super::*;
    use crate::files::PreviewBody;
    use crate::test_support::demo_app;

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// `train` is selected and the files pane has focus. Its files: 2 deps, params.yaml, model.txt, metrics.json.
    fn on_train_files() -> App {
        let mut app = demo_app();
        app.selected = 2;
        press(&mut app, KeyCode::Tab);
        app
    }

    #[test]
    fn tab_moves_focus_between_the_panes_and_back() {
        let mut app = demo_app();
        assert_eq!(app.focus, Focus::Stages);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Files);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Stages);
    }

    #[test]
    fn enter_on_a_stage_steps_into_its_files_and_esc_steps_back() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Files);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Stages);
    }

    #[test]
    fn j_and_k_move_the_file_cursor_and_stop_at_the_ends() {
        let mut app = on_train_files();
        assert_eq!(app.selected_files().len(), 5);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.file_cursor, 4);
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('k'));
        }
        assert_eq!(app.file_cursor, 0);
        assert_eq!(app.selected, 2, "the stage selection must not move");
    }

    #[test]
    fn changing_stage_resets_the_file_cursor() {
        let mut app = on_train_files();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.file_cursor, 1);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!((app.selected, app.file_cursor), (1, 0));
    }

    #[test]
    fn enter_on_a_file_asks_for_its_preview() {
        let mut app = on_train_files();
        press(&mut app, KeyCode::Char('j')); // data/features.csv
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(effects, [Effect::LoadPreview("data/features.csv".into())]);
    }

    #[test]
    fn dispatch_reads_the_file_and_opens_the_popup() {
        let mut app = on_train_files();
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('j')); // params.yaml
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        app.dispatch(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        );
        let preview = app.preview.as_ref().expect("popup open");
        assert_eq!(preview.path, "params.yaml");
        assert!(matches!(preview.body, PreviewBody::Text { .. }));
    }

    fn open_popup(app: &mut App, text: &str) {
        let body = PreviewBody::Text {
            text: text.into(),
            truncated: false,
        };
        app.update(Event::PreviewLoaded(Preview::new("a.txt".into(), body)));
    }

    #[test]
    fn the_popup_is_modal_q_closes_it_instead_of_quitting() {
        let mut app = demo_app();
        open_popup(&mut app, "x\n");
        press(&mut app, KeyCode::Char('e')); // would switch views
        press(&mut app, KeyCode::Char('r')); // would start a run
        assert_eq!(app.view, View::Pipeline);
        assert!(app.run.is_none());
        press(&mut app, KeyCode::Char('q'));
        assert!(app.preview.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_still_quits_with_the_popup_open() {
        let mut app = demo_app();
        open_popup(&mut app, "x\n");
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn popup_scrolls_within_the_text() {
        let mut app = demo_app();
        open_popup(&mut app, &"l\n".repeat(40));
        let scroll = |app: &App| app.preview.as_ref().unwrap().scroll;
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(scroll(&app), 1);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(scroll(&app), 16);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(scroll(&app), 39);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(scroll(&app), 39, "never past the last line");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(scroll(&app), 0);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(scroll(&app), 0, "never below zero");
    }

    #[test]
    fn o_requests_the_editor_for_an_existing_file() {
        let mut app = on_train_files();
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('j')); // params.yaml
        }
        let effects = press(&mut app, KeyCode::Char('o'));
        assert_eq!(effects, [Effect::OpenEditor("params.yaml".into())]);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.execute(effects.into_iter().next().unwrap(), &tx);
        let requested = app.editor_request.take().expect("editor requested");
        assert!(requested.ends_with("params.yaml"));
    }

    #[test]
    fn o_on_a_missing_file_explains_instead_of_opening_the_editor() {
        let (_dir, mut app) = crate::test_support::app_with_files(&[(
            "dvc.yaml",
            "stages:\n  a:\n    cmd: x\n    outs: [gone.txt]\n",
        )]);
        press(&mut app, KeyCode::Tab);
        let (tx, _rx) = std::sync::mpsc::channel();
        for effect in press(&mut app, KeyCode::Char('o')) {
            app.execute(effect, &tx);
        }
        assert!(app.editor_request.is_none());
        assert_eq!(app.notice.as_deref(), Some("gone.txt does not exist yet"));
    }

    #[test]
    fn a_stage_without_files_has_nothing_to_open() {
        let (_dir, mut app) =
            crate::test_support::app_with_files(&[("dvc.yaml", "stages:\n  a:\n    cmd: x\n")]);
        press(&mut app, KeyCode::Tab);
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(press(&mut app, KeyCode::Char('o')).is_empty());
    }

    #[test]
    fn running_still_works_while_the_files_pane_has_focus() {
        let mut app = on_train_files();
        let effects = press(&mut app, KeyCode::Char('r'));
        assert!(matches!(effects.as_slice(), [Effect::StartRun { .. }]));
    }
}

#[cfg(test)]
mod dag_view_tests {
    use super::*;
    use crate::test_support::{app_with_files, demo_app};

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    const DIAMOND: &str = "stages:\n  a:\n    cmd: x\n    outs: [a.txt]\n  b:\n    cmd: x\n    deps: [a.txt]\n    outs: [b.txt]\n  c:\n    cmd: x\n    deps: [a.txt]\n    outs: [c.txt]\n  d:\n    cmd: x\n    deps: [b.txt, c.txt]\n";

    #[test]
    fn g_opens_the_graph_and_g_or_esc_leaves_it() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.view, View::Dag);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.view, View::Pipeline);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Pipeline);
    }

    #[test]
    fn e_from_the_graph_goes_to_the_experiments() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('g'));
        let effects = press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.view, View::Experiments);
        assert_eq!(effects, [Effect::FetchExperiments]);
    }

    #[test]
    fn l_and_h_walk_along_the_edges_and_share_the_selection_with_the_list() {
        let mut app = demo_app(); // prepare -> features -> train
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.selected_stage().unwrap().name, "train");
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            app.selected_stage().unwrap().name,
            "train",
            "no further downstream"
        );
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.selected_stage().unwrap().name, "features");
    }

    #[test]
    fn j_and_k_move_within_a_layer() {
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", DIAMOND)]);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l')); // a -> b or c
        let first = app.selected_stage().unwrap().name.clone();
        press(&mut app, KeyCode::Char('j'));
        let second = app.selected_stage().unwrap().name.clone();
        assert_ne!(first, second);
        assert!(["b", "c"].contains(&first.as_str()) && ["b", "c"].contains(&second.as_str()));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_stage().unwrap().name, first);
    }

    #[test]
    fn enter_opens_the_stage_in_the_pipeline_view() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.view, View::Pipeline);
        assert_eq!(app.selected_stage().unwrap().name, "features");
    }

    #[test]
    fn r_and_capital_r_run_the_stage_under_the_cursor() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        let effects = press(&mut app, KeyCode::Char('r'));
        assert_eq!(
            effects,
            [Effect::StartRun {
                stage: "features".into(),
                with_upstream: false
            }]
        );
    }

    #[test]
    fn moving_in_the_graph_resets_the_file_cursor() {
        let mut app = demo_app();
        app.file_cursor = 2;
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.file_cursor, 0);
    }

    #[test]
    fn an_empty_project_survives_graph_keys() {
        let (_dir, mut app) = app_with_files(&[("dvc.yaml", "stages: {}\n")]);
        press(&mut app, KeyCode::Char('g'));
        for c in ['h', 'j', 'k', 'l'] {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(press(&mut app, KeyCode::Char('r')).is_empty());
    }

    #[test]
    fn the_graph_view_does_not_swallow_modal_keys_or_quit() {
        let mut app = demo_app();
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    const WITH_PLOTS: &[(&str, &str)] = &[
        (
            "dvc.yaml",
            "stages:\n  a:\n    cmd: echo\nplots:\n  - acc.csv:\n      x: epoch\n  - gone.csv\n",
        ),
        ("acc.csv", "epoch,acc\n1,0.5\n2,0.7\n3,0.9\n"),
    ];

    #[test]
    fn plots_view_loads_on_first_visit_and_moves_between_plots() {
        let (_dir, mut app) = app_with_files(WITH_PLOTS);
        assert!(app.plots.data.is_none());
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.view, View::Plots);
        assert_eq!(app.plots.data.as_ref().unwrap().as_ref().unwrap().len(), 2);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.plots.selected, 1);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.plots.selected, 0);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Pipeline);
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.view, View::Plots);
    }

    #[test]
    fn p_still_toggles_the_chart_in_the_experiments_view() {
        let (_dir, mut app) = app_with_files(WITH_PLOTS);
        app.view = View::Experiments;
        let before = app.experiments.plot;
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.view, View::Experiments);
        assert_ne!(app.experiments.plot, before);
    }

    #[test]
    fn n_adds_a_plot_to_dvc_yaml_selects_it_and_closes_the_form() {
        let (dir, mut app) = app_with_files(WITH_PLOTS);
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.plot_form.is_some());
        // Digits typed into the form do not switch views.
        for c in "loss.csv".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.plot_form.is_none());
        assert_eq!(app.plots.selected, 2);
        let yaml = std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap();
        assert!(yaml.contains("  - loss.csv\n"), "{yaml}");
        assert!(app.notice.as_deref().unwrap().contains("loss.csv"));
    }

    #[test]
    fn a_duplicate_plot_is_refused_and_the_form_stays_open() {
        let (dir, mut app) = app_with_files(WITH_PLOTS);
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('n'));
        for c in "gone.csv".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let before = std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap();
        press(&mut app, KeyCode::Enter);
        assert!(
            app.plot_form
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("already")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap(),
            before
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.plot_form.is_none());
    }

    #[test]
    fn d_toggles_the_definition_panel() {
        let (_dir, mut app) = app_with_files(WITH_PLOTS);
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('d'));
        assert!(app.plots.definition);
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.plots.definition);
    }

    #[test]
    fn ctrl_f_in_the_plot_form_requests_a_pick_and_the_choice_fills_the_file() {
        let (_dir, mut app) = app_with_files(WITH_PLOTS);
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('n'));
        app.update(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let request = app.pick_request.take().unwrap();
        assert_eq!(request.items, ["acc.csv"]);
        assert!(!request.multi);
        assert_eq!(request.target, PickTarget::PlotFile);
        app.file_picked(PickTarget::PlotFile, Ok(vec!["acc.csv".to_owned()]));
        assert_eq!(app.plot_form.as_ref().unwrap().file(), "acc.csv");
        app.file_picked(PickTarget::PlotFile, Ok(Vec::new()));
        assert_eq!(app.plot_form.as_ref().unwrap().file(), "acc.csv");
        app.file_picked(PickTarget::PlotFile, Err("fzf not found".to_owned()));
        assert_eq!(
            app.plot_form.as_ref().unwrap().error.as_deref(),
            Some("fzf not found")
        );
    }

    const CHAIN: &[(&str, &str)] = &[
        (
            "dvc.yaml",
            "stages:\n  a:\n    cmd: x\n    deps: [c.txt]\n    outs: [a.txt]\n  b:\n    cmd: y\n    deps: [a.txt]\n    outs: [b.txt]\n",
        ),
        ("params.yaml", "train:\n  lr: 0.1\n  epochs: 3\n"),
        ("data.csv", "x\n1\n"),
    ];

    fn type_into(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn n_adds_a_stage_to_dvc_yaml_selects_it_and_asks_for_new_statuses() {
        let (dir, mut app) = app_with_files(CHAIN);
        press(&mut app, KeyCode::Char('n'));
        assert!(app.stage_form.is_some());
        type_into(&mut app, "train");
        press(&mut app, KeyCode::Tab);
        type_into(&mut app, "python train.py");
        let effects = app.update(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(effects, [Effect::RefreshStatuses]);
        assert!(app.stage_form.is_none());
        assert_eq!(app.project.stages.len(), 3);
        assert_eq!(app.project.stages[2].name, "train");
        let selected = app.graph.order[app.selected];
        assert_eq!(app.project.stages[selected].name, "train");
        assert!(app.statuses.is_none());
        let yaml = std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap();
        assert!(
            yaml.ends_with("  train:\n    cmd: python train.py\n"),
            "{yaml}"
        );
    }

    #[test]
    fn a_duplicate_stage_or_a_cycle_is_refused_and_dvc_yaml_is_left_alone() {
        let (dir, mut app) = app_with_files(CHAIN);
        let before = std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap();
        let try_stage = |app: &mut App, name: &str, deps: &str, outs: &str| {
            press(app, KeyCode::Char('n'));
            type_into(app, name);
            press(app, KeyCode::Tab);
            type_into(app, "cmd");
            press(app, KeyCode::Tab);
            type_into(app, deps);
            press(app, KeyCode::Tab);
            press(app, KeyCode::Tab);
            type_into(app, outs);
            press(app, KeyCode::Enter);
        };
        try_stage(&mut app, "a", "", "");
        assert!(
            app.stage_form
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("already")
        );
        press(&mut app, KeyCode::Esc);
        try_stage(&mut app, "c", "b.txt", "c.txt");
        assert!(
            app.stage_form
                .as_ref()
                .unwrap()
                .error
                .as_deref()
                .unwrap()
                .contains("cycle")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dvc.yaml")).unwrap(),
            before
        );
        assert_eq!(app.project.stages.len(), 2);
    }

    #[test]
    fn ctrl_f_in_the_stage_form_offers_files_or_param_keys_and_adds_the_picks() {
        let (_dir, mut app) = app_with_files(CHAIN);
        press(&mut app, KeyCode::Char('n'));
        let ctrl_f = || Event::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        app.stage_form.as_mut().unwrap().active = 2; // deps
        app.update(ctrl_f());
        let request = app.pick_request.take().unwrap();
        assert!(request.multi);
        assert_eq!(request.target, PickTarget::Stage(Pick::Deps));
        assert!(
            request.items.contains(&"data.csv".to_owned())
                && !request.items.contains(&"dvc.lock".to_owned())
        );
        app.file_picked(request.target, Ok(vec!["data.csv".to_owned()]));
        assert_eq!(app.stage_form.as_ref().unwrap().texts[2], "data.csv");

        app.stage_form.as_mut().unwrap().active = 3; // params
        app.update(ctrl_f());
        let request = app.pick_request.take().unwrap();
        assert_eq!(request.items, ["train.epochs", "train.lr"]);
        app.file_picked(request.target, Ok(vec!["train.lr".to_owned()]));
        assert_eq!(app.stage_form.as_ref().unwrap().texts[3], "train.lr");
    }

    #[test]
    fn n_also_opens_the_stage_form_from_the_graph_view() {
        let (_dir, mut app) = app_with_files(CHAIN);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.stage_form.is_some());
    }
}
