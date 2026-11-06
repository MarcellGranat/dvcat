# dvc_tui

A minimal terminal UI for [DVC](https://dvc.org): browse a pipeline, see which stages are stale and why,
run a stage with a live log, and compare experiments. Written in Rust with [ratatui](https://ratatui.rs).

> **Status:** feature-complete for the plans in `openspec/changes/` (`add-dvc-tui` and `add-dvc-tui-insights`).
> Pipeline list, detail and files panes with preview and editor, stale detection (params, deps, cmd, outputs) with reasons,
> `foreach`/`matrix` stages, a stage graph, runs with a live log and cancel, and an experiments table with deltas,
> a chart, new/apply/remove/branch actions. The look is [Catppuccin Mocha](https://catppuccin.com) and needs a true-colour terminal.

## Quick start

```sh
just demo         # builds a Git copy of the demo project (3 experiments) on first use and opens it
just demo-plain   # the plain demo on a scratch copy (no Git, so no experiments)
just demo-reset   # rebuild the Git demo from scratch
just check        # fmt + clippy + tests
```

`just` finds `dvc` via `$DVC_TUI_DVC`, then `PATH`, then `~/projects/articleIVdb/.venv/bin/dvc` (edit the `dvc` line in `justfile`).

## Run

```sh
cargo run -- [PROJECT_DIR]      # PROJECT_DIR defaults to the current directory
```

`dvc` is looked up in `$DVC_TUI_DVC`, then `<PROJECT_DIR>/.venv/bin/dvc`, then `PATH`.
If none is found, pressing `r` explains where it looked.

## Keys

| Key        | Action                                   |
|------------|------------------------------------------|
| `j` / `k`  | move selection down / up (in the focused pane) |
| `Tab`      | switch focus between the stages and the files pane |
| `Enter`    | on a stage: step into its files; on a file: preview it |
| `o`        | *(files pane or preview)* open the file in `$VISUAL` / `$EDITOR` (`vi` if unset) |
| `Esc`      | close the preview; from the files pane back to the stages |
| `g` / `G`  | *(preview)* top / bottom                 |
| `q`        | quit                                     |
| `r`        | run the selected stage only (`dvc repro --single-item`) |
| `R`        | run it with stale upstream stages (`dvc repro`) |
| `x`        | cancel the running stage (from either view) |
| `PgUp` / `PgDn` | scroll the log                      |
| `e`        | switch between the pipeline and the experiments table |
| `g`        | show the stage graph (`g` or `Esc` to leave); `h`/`l` follow the edges, `j`/`k` move within a layer, `Enter` opens the stage, `r`/`R` run it |
| `h` / `l`  | *(experiments)* move the column cursor   |
| `s`        | *(experiments)* sort by the cursor column; again flips the direction |
| `m`        | *(experiments)* show metrics of experiments as differences from their baseline (`▲ +0.1867`) and back |
| `F5`       | *(experiments)* reload with `dvc exp show --json` |
| `n`        | *(experiments)* new experiment: a form with a name and every param, pre-filled from the row under the cursor |
| `a` / `d`  | *(experiments)* apply / remove the experiment under the cursor, after a yes/no question |
| `b`        | *(experiments)* save the experiment under the cursor as a Git branch (`dvc exp branch`); asks for the name |
| `p`        | *(experiments)* show / hide a chart of one metric across the experiments |
| `[` / `]`  | *(experiments)* previous / next metric in the chart |

**Apply and remove.** `a` runs `dvc exp apply <name>`: it overwrites params, lock file, metrics and outputs in your workspace.
DVC first saves your uncommitted changes (undo with `git reset --hard && git stash apply refs/exps/apply/stash`).
`d` runs `dvc exp remove <name>`, which cannot be undone unless the experiment was pushed. Both only work on experiment rows,
show DVC's output in the log, and reload the table afterwards.

**Branch.** `b` opens a prompt pre-filled with `<experiment>-branch`; names Git would reject are refused with a reason.
The branch is created but **not** checked out; the footer tells you the `git switch` command.

**Chart.** With the cursor on a numeric param column the chart plots the metric against that param (one point per experiment);
on any other column it plots the creation order. The experiment under the table cursor is the solid block.

**Comparing with the baseline.** In the experiments table, a param that differs from the experiment's baseline is highlighted.
`m` switches the metric cells to signed differences; the arrows show direction only (the tool does not know whether higher or lower is better).
The info line under the table shows the baseline's value for the cell under the cursor. Sorting always uses the real values.

**New experiment form.** `Tab`/`Shift+Tab` move between the fields, typing edits, `Ctrl+U` clears a field and `Enter` runs it.
Only the values you changed become `-S` overrides, and the command that will run is shown at the bottom of the form.
It runs `dvc exp run --temp` (an isolated copy, so your workspace is untouched), switches to the pipeline view to show the live log, and reloads the table when the run ends.

The files pane lists what the selected stage reads and writes (deps, param files, outs, metrics).
A preview shows at most the first 64 KiB of a file; directories are listed and binary files are not dumped.
While a preview is open it receives all keys (`q` closes it), except `Ctrl+C`, which still quits.

## Graph

`g` draws the stages as boxes in dependency layers, left to right, with a line and an arrowhead for each dependency.
Boxes carry the status glyph and colour of the stage list; the line under the graph describes the stage under the cursor.
The cursor is shared with the pipeline view (the same stage stays selected), and a graph larger than the screen scrolls to keep it in view.

## Run times

The log title shows how long a run has taken (live while it runs, fixed afterwards), and a stage's detail pane has a
`last run` line with the outcome and duration of its latest run from this session. DVC itself keeps no durations, so
runs made outside the interface, or before it was opened, have none.
A run in which DVC only reports `Stage '<name>' didn't change, skipping` is not recorded, since nothing ran.

## Templated stages

`foreach`, `matrix` and `vars` in `dvc.yaml` are expanded the way DVC does, so `each@us`, `listed@0`, `rich@a` and `grid@lin-1`
are listed, ordered, checked and runnable like any other stage. Supported references: `${name}`, `${item}`, `${key}`,
`${item.field}`, `${list[0]}`, from `vars` (inline mappings and YAML/JSON files, also per stage) and the default `params.yaml`;
`\${` is a literal. A stage that uses anything else (for example `${a:b}`, or an unknown name) is shown with a `⚠` and its reason.

## Status glyphs

A stage is stale when its cmd, params, dependencies **or recorded outputs** differ from `dvc.lock` (a deleted output shows
`missing output: <path>`, an edited one `modified output: <path>`). Outputs are compared by size first and hashed only when the
size matches; directories and files over 256 MiB are shown as `unverified`.

`✓` up to date · `●` stale (the detail pane says why) · `○` never run · `?` unverified (e.g. a directory dependency) · `…` still checking.

## Demo project

`tests/fixtures/demo` is a throwaway three-stage DVC project (`prepare` → `features` → `train`).
Rebuild its DVC state (needs `dvc` on `PATH`, or set `DVC=/path/to/dvc`):

```sh
tests/fixtures/demo/regen.sh
```

The demo is not a Git repository, so the experiments view shows DVC's "Only supported for Git repositories".
To try experiments, build a Git copy with three finished experiments (`lr-001`, `lr-005`, `lr-010`):

```sh
DVC=/path/to/dvc tests/fixtures/demo/make-git-demo.sh /tmp/dvc-tui-git-demo
cargo run -- /tmp/dvc-tui-git-demo
```

## Develop

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/dvc_cross_check.rs` compares our stale detection with the real `dvc status --json`.
It needs DVC: `DVC_BIN=/path/to/dvc cargo test --test dvc_cross_check`;
`tests/git_demo.rs` builds the Git demo and reads its experiments the same way
(without DVC it prints `SKIPPED` and passes).
