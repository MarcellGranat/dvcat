# Proposal

## Why

DVC is CLI-only: understanding a pipeline means juggling `dvc dag`, `dvc status`, `dvc exp show` and reading `dvc.yaml`/`dvc.lock` by hand, and running one stage means remembering flags. A minimal, modern terminal UI can show the pipeline, what is stale and why, run a stage with a live log, and compare experiments, all in one place. It is also a deliberately scoped Rust learning project.

## What Changes

- New Rust binary `dvc_tui` (ratatui + crossterm) that takes a DVC project path (default: current directory).
- Pipeline view: stages from `dvc.yaml` in topological order, dependency edges derived from stage `outs` → `deps`, and a detail pane (cmd, deps, params, outs).
- Stale detection: compare `dvc.lock` against the workspace (params first, then dep hashes) and show why a stage is stale, e.g. `params.download.limit 3 → null`.
- Stage runs: launch `dvc repro <stage>` from the UI, stream its output live, allow cancelling.
- Experiments table: parse `dvc exp show --json`, show params and metrics per experiment, sortable.
- Out of scope for v1: graphical DAG drawing, plots, remotes/push/pull, editing `dvc.yaml`, `exp apply`/`branch` mutations.

## Capabilities

### New Capabilities
- `pipeline-view`: Browsing stages of a DVC project as a dependency-ordered list with a per-stage detail pane.
- `stale-detection`: Determining and explaining whether a stage is up to date by diffing `dvc.lock` against the workspace.
- `stage-run`: Launching a stage with `dvc repro`, streaming its output, and cancelling it.
- `experiments-table`: Listing experiments with params and metrics from `dvc exp show --json`.

### Modified Capabilities

## Impact

- New cargo project in this repo (currently empty); no existing code affected.
- Dependencies: `ratatui`, `crossterm`, `serde`, a maintained YAML crate, `serde_json`, `anyhow`; `md-5` for dependency hashing.
- Runtime dependency on the `dvc` CLI, located on `PATH` or in the project's `.venv/bin`.
- Test fixtures: a throwaway three-stage DVC repo with a metrics file; `~/projects/articleIVdb` is used read-only for manual checks.
