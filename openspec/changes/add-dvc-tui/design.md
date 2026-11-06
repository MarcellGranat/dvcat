# Design

## Context

Greenfield cargo project in an empty repo. Requirements are in `specs/`; motivation in `proposal.md`. Facts established while exploring a real-world project:

- `dvc dag` has no JSON output (only dot/mermaid/md), but `dvc.yaml` contains enough to build the graph.
- `dvc.lock` stores cmd, dep hashes, param values and out hashes per stage, so staleness for cmd, params and file deps can be computed without invoking DVC. Real example: lock has `download.limit: 3`, `params.json` has `null`.
- `dvc exp show --json` returns a list of revisions `{rev, name, data: {params, metrics, deps, outs}, experiments: [...]}`, with params nested by file (`params.json` → `data` → keys).
- `dvc` may be installed only in the project's `.venv/bin`.
- The author is learning Rust: the structure should favour simple ownership (owned data, indices instead of references) and few moving parts.

## Goals / Non-Goals

**Goals:**
- A small, readable codebase where each module introduces one idea (parsing, graph, UI loop, subprocess).
- The UI thread never blocks on `dvc` or on disk-heavy work.
- Pure logic (parsing, topo sort, stale diff, table building) testable without a terminal or `dvc`.

**Non-Goals:**
- Reimplementing DVC's full status semantics (directory hashes, remotes, `foreach`/`matrix` stages in v1).
- Async runtime, plugins, configuration files, theming.

## Decisions

### 1. Elm-style loop: one `App` state, an `Event` enum, `update` and `view`
All input (keys, ticks, job output, fetch results) becomes an `Event` on one `std::sync::mpsc` channel. `update(&mut App, Event)` is the only mutation point; `view(&App, &mut Frame)` only reads.
*Alternatives:* component-owned state with callbacks (borrow-checker friction, scattered mutation); an async actor model (extra concepts for little gain).

### 2. Threads and channels, no tokio
Each background job (a `dvc repro` run, an `exp show` fetch) is a `std::thread` that sends events over the channel. A separate thread forwards crossterm key events. A tick thread is unnecessary: the main loop uses `recv_timeout`.
*Alternatives:* tokio (powerful, but async is a second learning curve and the workload is a few processes).

### 3. Parse YAML ourselves, shell out for anything stateful
`dvc.yaml`, `dvc.lock` and param files are read with serde. `dvc repro` and `dvc exp show --json` are invoked as subprocesses. No attempt to link or embed DVC.
*Alternatives:* run `dvc status --json` for everything (slow, takes DVC's repo lock, blocks while a run is active); a DVC Python bridge (heavy, brittle).

### 4. Graph as `Vec<Stage>` plus indices
Stages are stored in a `Vec`, edges are `Vec<Vec<usize>>` (dependencies by index), and topological order is computed once with Kahn's algorithm, which also detects cycles. Edge rule: stage B depends on A when a dep path of B equals or is nested under an out path of A (path-prefix match on normalized relative paths).
*Alternatives:* `petgraph` (extra dependency for ~40 lines of logic); a graph of `Rc<RefCell<Stage>>` or references (fights the borrow checker for no benefit).

### 5. Stale detection is a pure function returning reasons
`fn stale_reasons(stage, lock_entry, workspace) -> Vec<Reason>` where `Reason` is an enum (`NeverRun`, `CmdChanged`, `ParamChanged{key, old, new}`, `DepChanged`, `DepMissing`, `DirUnverified`, `Upstream(name)`). Status is derived: no reasons and not never-run = up to date. Upstream propagation runs over the topological order. File hashing uses `md-5`; directory deps are reported as `DirUnverified` rather than guessed.
*Alternatives:* a boolean stale flag (loses the "why", which is the feature's value).

### 6. Locating `dvc`
Resolution order: `DVCAT_DVC` env var, `<project>/.venv/bin/dvc`, then `PATH`. Failure lists the places searched.

### 1b. Side effects as data
`update` returns `Vec<Effect>` (`StartRun`, `CancelRun`, `RefreshStatuses`) instead of performing I/O; `App::dispatch` executes them. Key handling and the single-run rule are therefore testable without spawning anything, and tests inject a fake `dvc` through a `dvc_lookup` function pointer rather than process-wide environment variables.

### 7. Process control
Spawn through a small `sh` wrapper (own process group) that appends stdout and stderr to `.dvc/tmp/dvcat-run.log` (`.dvcat/` when there is no `.dvc`) and writes the exit status to `dvcat-run.exit`; a record of the run (arguments, kind, start time, process group) goes to `dvcat-run.json`. A follower thread reads the log every 40 ms, sends new text as events, and announces the outcome when the exit file appears (or the group is found gone). Because nothing depends on a pipe or on the interface being alive, the run survives the interface quitting, and `runner::attach` can follow or report it in the next session; the wrapper's command line carries a marker, so a process id that now belongs to something else is never followed or signalled. That `/proc` check is used only for runs found from an earlier session: a process this session spawned is trusted (we hold the child and know when it exits), because on a busy machine a freshly started process can show an empty command line for a few hundred milliseconds. Lines are split on `\n` and `\r`, and a `\r`-terminated line replaces the previous log line instead of appending. The child is started in its own process group so cancel can signal the whole group (`SIGTERM`, then `SIGKILL` after a timeout); `CommandExt::process_group(0)` creates the group. The standard library cannot signal a group, so the `kill -s <SIG> -- -<pgid>` program is used (no `libc`, no `unsafe`). The child gets `PYTHONUNBUFFERED=1` so Python stages writing to a file stream output instead of buffering until exit.
*Alternatives:* a PTY (faithful progress bars, but heavy); `--quiet` only (loses information).

### 8. Experiments parsing
Typed serde structs for the parts of `exp show --json` we use, with `Option` and `#[serde(default)]` for everything that may be absent; unknown fields are ignored. The table model (columns, rows, sort) is built from these in a pure module.

### 8b. Experiments view
`dvc exp show --json` is fetched on a background thread (first visit to the view, then `F5`) and arrives as `Event::Experiments(Result<Table, String>)`. The previous table stays on screen while a refresh runs. The fetch is refused while a stage runs, because `dvc repro` holds the repository lock. Only experiments are sorted (within their baseline); the workspace and baseline rows stay pinned. Numbers sort numerically, everything else as text, empty cells always last. Columns that do not fit are windowed around the cursor, with the name column pinned. Cancelling (`x`) works from both views.

### 8c. Theme, files pane and editor handoff
`theme.rs` holds the Catppuccin Mocha palette as `Color::Rgb` constants and the few shared styles, so no other module names a colour. The pipeline view has two focusable panes (`Focus::{Stages, Files}`, toggled with `Tab`); the files pane lists `files::stage_files`. A preview is `Effect::LoadPreview` -> `Event::PreviewLoaded`, read synchronously but capped at 64 KiB (`files::PREVIEW_LIMIT`); while it is open it is modal and takes every key but `Ctrl+C`. `o` does not touch the terminal from `update`: the effect stores `editor_request`, and `main` suspends the UI (`ratatui::restore`), runs `$VISUAL`/`$EDITOR`, re-enters raw mode and the alternate screen, and re-checks statuses. The key thread uses `event::poll` and an `Arc<AtomicBool>` pause flag so it cannot steal the editor's keystrokes.
*Alternatives:* a built-in editor (large scope); preview only (no way to edit params); a theme config file (non-goal in v1).
Git demo: the fixture cannot contain a `.git`, so `make-git-demo.sh` builds a Git copy with three `dvc exp run --temp` experiments (`--temp` isolates them from each other's params).

### 8d. New experiment form
`form.rs` holds the form as pure state: a name and a `FormField` (label `file:key`, original, value) per param column, with `on_key` returning a `FormAction` (`Continue`, `Cancel`, `Submit{label, args}`). Only changed fields become `-S` overrides, built by `runner::exp_run_args`, which always passes `--temp` so runs do not inherit each other's params or touch the workspace. Submitting stores the arguments in `App::run` (`Run` now carries its `args`, so stage runs and experiment runs share one start/stream/cancel path) and switches to the pipeline view, where the log pane lives. When an experiment run ends, the app asks for a fresh `exp show` (the repository lock is free again). Like the preview, the form is modal.
*Alternatives:* free-text `key=value` input (error-prone, no discovery of param names); a separate log pane in the experiments view (more layout for little gain).

### 8e. Apply and remove
`a` and `d` build a `Confirm` (title, explanation, label, `dvc` arguments); like the preview and the form it is modal, and `y`/Enter hands the arguments to the same `begin_dvc_run` path the form uses, so output streaming, cancel and the table reload are shared. `dvc exp apply` is run without `--no-force`: in DVC 3.67 that flag is deprecated and no longer protects anything; apply always overwrites the workspace and saves the previous state to `refs/exps/apply/stash`, which the question mentions. Removal is irreversible, which the question says.

### 9. Dependencies
`ratatui` (re-exports `crossterm`), `serde` + `serde_json`, one maintained YAML crate for serde, `anyhow`, `md-5`. Dev: `tempfile`. A panic hook and a drop guard restore the terminal on every exit path.

### 10. Module layout
```
src/
  main.rs        thin binary: load project, terminal setup/teardown, loop
  lib.rs         module root (library crate, so tests/ can use everything)
  cli.rs         argument parsing
  app.rs         App state, Event, update
  ui.rs          view functions per screen
  project.rs     dvc.yaml / dvc.lock / params loading (serde types)
  graph.rs       edges, topo sort, cycle detection
  stale.rs       Reason enum, stale_reasons, propagation
  runner.rs      dvc lookup, spawn, line streaming, cancel
  experiments.rs exp show types, table model, sorting
tests/fixtures/  throwaway three-stage DVC repo
```

## Risks / Trade-offs

- [Own stale logic disagrees with DVC] → Treat it as advisory, flag unverifiable cases, add a later option to cross-check with `dvc status --json`; fixture tests compare against real `dvc status` output.
- [`dvc repro` holds the repo lock; other `dvc` calls fail meanwhile] → Disable experiments refresh during a run and show why.
- [DVC progress output uses `\r` and ANSI codes] → Split on `\r`, strip ANSI sequences; keep scrollback bounded.
- [Our md5 differs from DVC's for text files with CRLF line endings, since DVC normalises them] → Known gap: such a dep may show as changed; `tests/dvc_cross_check.rs` is the guard, and a later task can add normalisation.
- [Modified or missing *outputs* are not checked, though `dvc status` reports them] → Out of scope for v1; the stage view says "up to date" about inputs only.
- [Propagating staleness downstream is conservative: `dvc repro` skips a downstream stage if the upstream output turns out unchanged] → Intentional per spec; the reason text says "upstream: <stage>".
- [DVC spawns a detached `dvc daemon analytics` process after commands] → Not ours to manage; it appears after a plain `dvc status` as well. Users can set `DVC_NO_ANALYTICS=1`.
- [Unix-only process groups] → Documented v1 limitation.
- [`foreach`/`matrix` stages, `vars`, `${}` interpolation] → Not expanded in v1; such stages are shown as-is and flagged unsupported.
- [Large `dvc.lock` or many stages] → Hashing runs off the UI thread; results arrive as events.

## Open Questions

- Cancel sends SIGTERM, then SIGKILL after 3 s. In ~20 cancel trials against real DVC (both TERM and INT, at varied moments) `dvc.lock` always stayed valid, but one early manual run showed `'./dvc.lock' validation failed` on the next run after a cancel and could not be reproduced. If it recurs, switch to an INT → TERM → KILL ladder (INT is what Ctrl+C in a terminal sends and lets DVC and user code clean up).

- ~~Which YAML crate to use~~ Resolved: `serde_norway` (maintained fork of the archived `serde_yaml`, same API).
- Whether `x` (cancel) should ask for confirmation.
