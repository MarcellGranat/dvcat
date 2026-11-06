//! Application state and the one place that changes it.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    dag::{self, Layout, Move},
    experiments::{self, RowKind, Sort, Table},
    files::{self, FileEntry, Preview},
    form::{ExperimentForm, FormAction},
    graph::Graph,
    log::LogBuffer,
    project::{Project, Stage},
    prompt::{Prompt, PromptAction},
    runner::{self, DvcNotFound, KILL_GRACE, RunOutcome, RunningJob},
    stale::{self, StageStatus},
};

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

/// A yes/no question shown before something destructive. Modal, like the preview and the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    /// Names the run in the UI once confirmed.
    pub label: String,
    pub args: Vec<String>,
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
}

impl ExperimentsView {
    pub fn table(&self) -> Option<&Table> {
        self.data.as_ref()?.as_ref().ok()
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
    /// Read a project file (path relative to the project root) for the preview popup.
    LoadPreview(String),
    /// Hand a project file to `$EDITOR`. The terminal belongs to `main`, so we only leave a request.
    OpenEditor(String),
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
    job: Option<RunningJob>,
}

impl Run {
    pub fn new(stage: String, with_upstream: bool) -> Self {
        let args = runner::repro_args(&stage, with_upstream);
        Self::with_args(stage, args, with_upstream, RunKind::Stage)
    }

    /// An experiment run; `label` is shown in the header.
    pub fn experiment(label: String, args: Vec<String>) -> Self {
        Self::with_args(label, args, false, RunKind::Experiment)
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
            job: None,
        }
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
    pub focus: Focus,
    /// Where the graph view draws each stage; computed once, because the stages do not change.
    pub dag: Layout,
    /// The open "new experiment" form, if any. Like the preview, it is modal.
    pub form: Option<ExperimentForm>,
    pub confirm: Option<Confirm>,
    pub prompt: Option<Prompt>,
    /// Cursor in the files pane, an index into [`App::selected_files`].
    pub file_cursor: usize,
    /// The open preview popup, if any. While it is open it receives all keys.
    pub preview: Option<Preview>,
    /// Set when the user asked for the editor; `main` takes it, suspends the UI and runs the editor.
    pub editor_request: Option<PathBuf>,
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
            focus: Focus::Stages,
            form: None,
            confirm: None,
            prompt: None,
            file_cursor: 0,
            preview: None,
            editor_request: None,
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
            Event::Experiments(result) => {
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
        if self.confirm.is_some() {
            return self.on_confirm_key(key.code);
        }
        if self.prompt.is_some() {
            return self.on_prompt_key(key);
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
            KeyCode::Char('g') => {
                self.view = if self.view == View::Dag {
                    View::Pipeline
                } else {
                    View::Dag
                };
                Vec::new()
            }
            // Stopping a run makes sense from either view.
            KeyCode::Char('x') => self.request_cancel(),
            code => match self.view {
                View::Pipeline => self.on_pipeline_key(code),
                View::Experiments => self.on_experiments_key(code),
                View::Dag => self.on_dag_key(code),
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
            KeyCode::Char('r') => return self.request_run(false),
            KeyCode::Char('R') => return self.request_run(true),
            _ => {}
        }
        Vec::new()
    }

    fn toggle_view(&mut self) -> Vec<Effect> {
        match self.view {
            View::Experiments => {
                self.view = View::Pipeline;
                Vec::new()
            }
            View::Pipeline | View::Dag => {
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
        match code {
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
            KeyCode::Char('n') => self.open_experiment_form(),
            KeyCode::Char('b') => self.open_branch_prompt(),
            KeyCode::Char('a') => self.ask_about_selected_experiment(Action::Apply),
            KeyCode::Char('d') => self.ask_about_selected_experiment(Action::Remove),
            KeyCode::F(5) => return self.request_experiments(),
            KeyCode::Esc => self.view = View::Pipeline,
            _ => {}
        }
        self.clamp_experiments_cursor();
        Vec::new()
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
            },
            Action::Remove => Confirm {
                title: format!("Remove {name}?"),
                lines: vec![
                    "Deletes the experiment from this repository.".into(),
                    "This cannot be undone (unless it was pushed to a remote).".into(),
                ],
                label: format!("remove {name}"),
                args: runner::exp_remove_args(&name),
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
            PromptAction::Submit(branch) => {
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
                Some(Confirm { label, args, .. }) => self.begin_dvc_run(label, args),
                None => Vec::new(),
            },
            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                self.confirm = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Start a `dvc` command that works on experiments, showing its log in the pipeline view.
    /// When it ends the experiments table is reloaded (see `on_run_finished`).
    fn begin_dvc_run(&mut self, label: String, args: Vec<String>) -> Vec<Effect> {
        self.run = Some(Run::experiment(label, args));
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
        let Some(column) = table.columns().into_iter().nth(self.experiments.column) else {
            return;
        };
        self.experiments.sort.choose(column);
    }

    /// Keep the cursor on an existing row and column (the table may have shrunk or be absent).
    fn clamp_experiments_cursor(&mut self) {
        let (rows, columns) = self
            .experiments
            .table()
            .map_or((0, 0), |table| (table.rows.len(), table.columns().len()));
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
        run.phase = RunPhase::Finished(outcome);
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
            Effect::FetchExperiments => self.fetch_experiments(tx),
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
        let Some(args) = self.run.as_ref().map(|run| run.args.clone()) else {
            return;
        };
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
                .map(|log| log.lines().filter(|l| *l == "exp show --json").count())
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
        assert_eq!(calls(&h), ["exp show --json"], "only the initial fetch");
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
        assert_eq!(calls(&h), ["exp show --json"]);
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
}
