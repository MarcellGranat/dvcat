# Proposal

## Why

`add-dvc-tui` made the pipeline browsable, runnable and comparable, but it still says "up to date" when an output was deleted, shows experiment numbers without saying how they differ from the baseline, can't show the graph as a graph, can't keep a good experiment (apply works, but promoting it to a Git branch needs the CLI), and flags `foreach`/`matrix` stages as unsupported — which is how many real projects define their stages. These six gaps are what a user hits first once the basics work.

## What Changes

- **Output check:** stale detection also compares `outs` with `dvc.lock` (missing, size or md5 differs), so a deleted or hand-edited output makes the stage stale, as `dvc status` already reports.
- **Metrics diff:** the experiments table marks params that differ from the experiment's baseline and can show metrics as deltas (`+0.1867`) instead of absolute values.
- **Plot panel:** a chart under the experiments table with a chosen metric on Y and either the cursor param or the creation order on X.
- **Experiment branch:** `b` on an experiment row asks for a branch name (default `<experiment>-branch`) and runs `dvc exp branch`, showing the output in the log.
- **DAG view:** a third view, `g`, that draws the stages as boxes in dependency layers with connecting lines, coloured by status; `Enter` on a box jumps to that stage.
- **foreach/matrix stages:** `dvc.yaml` templates are expanded into the real stages (`name@key`) with `${item}`, `${key}`, `${item.field}`, matrix variables and `vars:`, so they are listed, ordered, checked and runnable like any other stage.
- No breaking changes. Stages that use interpolation we cannot resolve stay flagged as unsupported.

## Capabilities

### New Capabilities
- `output-check`: Detecting missing or modified stage outputs by comparing the workspace with `dvc.lock`, and explaining which output differs.
- `metrics-diff`: Showing how an experiment's params and metrics differ from its baseline in the experiments table.
- `metrics-plot`: Charting one metric across experiments against a param or creation order.
- `experiment-branch`: Promoting an experiment to a Git branch from the experiments view.
- `dag-view`: Drawing the stage graph with status colours and jumping from a node to its stage.
- `matrix-stages`: Expanding `foreach`, `matrix` and `vars` in `dvc.yaml` into concrete stages.

### Modified Capabilities

None. The capabilities from `add-dvc-tui` (`pipeline-view`, `stale-detection`, `stage-run`, `experiments-table`) are not archived yet, so their behaviour is extended here through new capabilities rather than deltas. If that change is archived first, `output-check` could instead become a modification of `stale-detection`.

## Impact

- Code: `stale.rs` (outs), `project.rs` (expansion before `Stage` parsing), `experiments.rs` and `ui/experiments.rs` (diff, plot), `runner.rs` (`exp branch` arguments), a new `ui/dag.rs` and `dag.rs` (layout), `app.rs` (views, keys, one new modal).
- Dependencies: none expected; ratatui's `Chart` and `Sparkline` cover plots, and the DAG layout is plain Rust.
- Runtime: `dvc exp branch` needs a Git repository; the output check reads and hashes output files, so large outputs are size-checked first and hashed only up to a size cap.
- Tests: the real-DVC cross-check grows scenarios for deleted and edited outputs; new fixtures cover `foreach`, `matrix` and `vars`.
