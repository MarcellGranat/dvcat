# Design

## Context

Greenfield cargo project in an empty repo. Requirements are in `specs/`; motivation in `proposal.md`. Facts established while exploring `~/projects/articleIVdb`:

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
Resolution order: `DVC_TUI_DVC` env var, `<project>/.venv/bin/dvc`, then `PATH`. Failure lists the places searched.

### 7. Process control
Spawn with piped stdout and stderr, merged by two reader threads into one event stream. Lines are split on `\n` and `\r`, and a `\r`-terminated line replaces the previous log line instead of appending. The child is started in its own process group so cancel and quit can signal the whole group (`SIGTERM`, then `SIGKILL` after a timeout); this needs the `libc` crate or `std::os::unix::process::CommandExt::process_group`, the latter preferred to avoid a dependency (Unix only in v1).
*Alternatives:* a PTY (faithful progress bars, but heavy); `--quiet` only (loses information).

### 8. Experiments parsing
Typed serde structs for the parts of `exp show --json` we use, with `Option` and `#[serde(default)]` for everything that may be absent; unknown fields are ignored. The table model (columns, rows, sort) is built from these in a pure module.

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
- [Unix-only process groups] → Documented v1 limitation.
- [`foreach`/`matrix` stages, `vars`, `${}` interpolation] → Not expanded in v1; such stages are shown as-is and flagged unsupported.
- [Large `dvc.lock` or many stages] → Hashing runs off the UI thread; results arrive as events.

## Open Questions

- ~~Which YAML crate to use~~ Resolved: `serde_norway` (maintained fork of the archived `serde_yaml`, same API).
- Whether `x` (cancel) should ask for confirmation.
