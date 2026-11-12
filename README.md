# dvcat 🐱

*DVC + cat: a catppuccin-coloured terminal UI that keeps an eye on your pipeline.*

A minimal terminal UI for [DVC](https://dvc.org): browse a pipeline, see which stages are stale and why,
run a stage with a live log, and compare experiments. Written in Rust with [ratatui](https://ratatui.rs).

> **Status:** feature-complete for the plans in `openspec/changes/` (`add-dvc-tui` and `add-dvc-tui-insights`).
> Pipeline list, detail and files panes with preview and editor, stale detection (params, deps, cmd, outputs) with reasons,
> `foreach`/`matrix` stages, a stage graph, runs with a live log and cancel, and an experiments table with deltas,
> a chart, new/apply/remove/branch actions. The look is [Catppuccin Mocha](https://catppuccin.com) and needs a true-colour terminal.

## Install

```sh
cargo install --git https://github.com/MarcellGranat/dvcat --locked
# or from a clone: cargo install --path . --locked
```

Then, in any DVC project: `dvcat` (or `dvcat path/to/project`).

## Quick start

```sh
just demo         # builds a Git copy of the demo project (3 experiments) on first use and opens it
just demo-plain   # the plain demo on a scratch copy (no Git, so no experiments)
just demo-reset   # rebuild the Git demo from scratch
just check        # fmt + clippy + tests
```

`just` uses the `dvc` named by `$DVCAT_DVC`, or else the one on your `PATH`.

## Run

```sh
cargo run -- [PROJECT_DIR]      # PROJECT_DIR defaults to the current directory
```

`dvc` is looked up in `$DVCAT_DVC`, then `<PROJECT_DIR>/.venv/bin/dvc`, then `PATH`.
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
| `q`        | quit; a running stage **keeps running** and is picked up again when you start `dvcat` in the project |
| `n`        | *(pipeline, graph)* add a stage: a form for name, cmd, deps, params, outs and metrics, with `[x]` options for `cache`, `persist`, `always_changed` and `frozen` (`Space` toggles). `Ctrl+F` on a list field picks files, or param keys, with [`fzf`](https://github.com/junegunn/fzf). The stage is written to `dvc.yaml` (nothing else in the file changes) |
| `r`        | run the selected stage only (`dvc repro --single-item`) |
| `R`        | run it with stale upstream stages (`dvc repro`) |
| `x`        | cancel the running stage (from either view) |
| `X`        | stop a `dvc` process started outside dvcat (asks first) |
| `P` / `U`  | `dvc pull` / `dvc push` (from any view). The remotes come from `dvc remote list`: with one it asks yes/no straight away, with several you pick one first (`j`/`k`, `Enter`; the cursor starts on the default). In the question, `t` adds the selected stage as the target (only its data; pipeline and graph views) `c` adds `--run-cache`, and `p` types more targets (paths, stages, `.dvc` files; `Enter` ends the editing). It runs `dvc pull -r NAME [STAGE] [--run-cache] [TARGETS...]`, shows the log in the pipeline view and re-checks the stage statuses afterwards |
| `PgUp` / `PgDn` | scroll the log                      |
| `1` `2` `3` `4` | pipeline / graph / experiments / plots view (also `g` for the graph, `e` for the experiments, `p` for the plots) |
| `e`        | switch between the pipeline and the experiments table |
| `g`        | show the stage graph (`g` or `Esc` to leave); `h`/`l` follow the edges, `j`/`k` move within a layer, `Enter` opens the stage, `r`/`R` run it |
| `h` / `l`  | *(experiments)* move the column cursor   |
| `s`        | *(experiments)* sort by the cursor column; again flips the direction |
| `Tab` / `Shift+Tab` | *(experiments)* switch between **Params**, **Metrics** and **All**; the title shows which is active and how many columns of how many are on screen. The table starts on the Metrics |
| *(age)*    | *(experiments)* the `age` column next to the name says how long ago each revision was made (`just now`, `5m ago`, `3h ago`, `2d ago`, `3w ago`, `4mo ago`); `s` on it sorts by time, oldest first, then newest first |
| `u`        | *(experiments)* show / hide the columns that hold the same value in every row (hidden at first) |
| `/`        | *(experiments)* filter the columns: only labels that contain all the typed words (any case) stay; empty clears it, `Esc` in the table clears it too |
| `{` / `}`  | *(experiments)* jump to the previous / next group of columns. The line above the headers names the group (`params.yaml:train`, `metrics/accuracy.json:by_fold.0`), the header is the key inside it |
| `Space` / `,` | *(experiments)* mark the row under the cursor / mark every row with the same value as the cursor cell (works on any param or metric column, like VisiData's `,`). Marked rows are yellow with a `●` |
| `z`        | *(experiments)* show only the marked experiments (the workspace and the baselines stay, as the comparison). With nothing marked it marks the rows like the cursor cell first. `z` again shows all |
| `Esc`      | *(experiments)* undoes one layer at a time: the `z` filter, then the marks, then the `/` column filter, then leaves the view |
| `-` / `+`  | *(experiments)* hide the param or metric column under the cursor (`H` does the same) / show all hidden columns again |
| `Enter`    | *(experiments)* focus on the experiment under the cursor: all its params and metrics in a list, grouped, next to the baseline's value and the change. A `♛` marks the metrics in which it is the best of all rows (ties count). `j`/`k` scroll, `h`/`l` show the previous / next row, `Tab`, `/` and `u` work as in the table, `Esc`, `Enter` or `q` go back |
| `m`        | *(experiments)* show metrics of experiments as differences from their baseline (`▲ +0.1867`) and back |
| `t`        | *(experiments)* transpose: experiments across the top, params and metrics down the side (`h`/`j`/`k`/`l` follow the screen) |
| `?` `:` `Ctrl+P` | the **command palette**: every action of the current screen with its key. Type words to search, `↑`/`↓` to choose, `Enter` runs it (`Esc` closes). `:messages` lists this session's notices, `:theme` switches mocha / latte / colour-blind |
| `T`        | *(experiments)* mark the best N rows (asked, 5 by default) of the metric under the cursor; then `z` shows only them |
| `B`        | *(experiments)* which way the metric under the cursor improves: read from its name, lower or higher (saved in `.dvc/tmp`; the config file can set it too) |
| `=`        | *(experiments)* compare two rows: the two marked ones, or the marked one and the cursor row. Only params and metrics that differ are listed, with the change and a `♛` on the one that is ahead; `x` swaps the sides |
| `f`        | *(experiments)* pin the column under the cursor to the left (it stays on screen however far you scroll, and ignores the scope and filter); again unpins |
| `F`        | *(experiments)* numbered metrics (`by_fold.0.rmse`, `by_fold.1.rmse`, ...) are folded into one column with the mean and the spread (`0.1234 ±0.012`); `F` shows them one by one. Long decimals in cells are rounded to four significant digits |
| `z`        | *(pipeline)* a group of stages (`split@0`, `split@1`, ... from `foreach`/`matrix`) with three or more members starts folded into one row (`▸ split ×10`); `z` opens or folds it |
| `y` / `Y`  | *(experiments)* copy the cell / the whole row as JSON to the clipboard (`pbcopy`, `wl-copy`, `xclip`, `xsel`, or the terminal's OSC 52) |
| `E`        | *(experiments)* write the table as it is sorted and filtered to `dvcat-experiments.csv` in the project |
| `o`        | on a stage: open its script (or params file) in `$EDITOR`; *(experiments)* open the file of the column under the cursor |
| `F5`       | *(experiments)* reload with `dvc exp show --json -A` |
| `n`        | *(experiments)* new experiment: a form with a name and every param, pre-filled from the row under the cursor |
| `a` / `d`  | *(experiments)* apply / remove the experiment under the cursor, after a yes/no question |
| `c`        | *(experiments)* edit the message of the experiment under the cursor (the text of `dvc exp run -m`); it is shown under the table |
| `b`        | *(experiments)* save the experiment under the cursor as a Git branch (`dvc exp branch`); asks for the name |
| `p`        | *(experiments)* show / hide a chart of one metric across the experiments |
| `v`        | *(experiments)* choose which params are shown: a list grouped by file and group (`train.*`); `Space` hides or shows a whole group, `a` shows all, `Esc` closes |
| `[` / `]`  | *(experiments)* previous / next metric in the chart |

**Apply and remove.** `a` runs `dvc exp apply <name>`: it overwrites params, lock file, metrics and outputs in your workspace.
DVC first saves your uncommitted changes (undo with `git reset --hard && git stash apply refs/exps/apply/stash`).
`d` runs `dvc exp remove <name>`, which cannot be undone unless the experiment was pushed. Both only work on experiment rows,
show DVC's output in the log, and reload the table afterwards.

**Messages.** The text given with `dvc exp run -m "..."` is the message of the experiment's Git commit; the table shows it under the rows. `c` edits its first line (the rest is kept). DVC has no command for this, so dvcat writes a copy of the commit with the new message and moves the experiment's ref to it: the name stays, the commit id changes.

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

## Config

`.dvcat.yaml` in the project directory (everything is optional):

```yaml
theme: latte            # mocha (default), latte or colour-blind
mouse: true             # click rows, headers and stages; the wheel scrolls (takes over text selection; Shift+drag still selects in most terminals)
better:                 # which way a metric improves: its label, or a part of its name
  coverage_90: higher
  gap: lower
```

The experiments table also remembers how you left it (scope, filter, pinned columns, sort, deltas, folded series, hidden columns, metric directions) in `.dvc/tmp/dvcat-history.json` and restores it next time.

`R` (run a stage with its upstream) asks first: the command, the stages that are not up to date and will run, and about how long they took the last time. While a stage runs the header shows the time so far next to that estimate.

Notices fade after a few seconds; `:messages` in the palette lists them all.


## Graph

`g` draws the stages as boxes in dependency layers, left to right, with a line and an arrowhead for each dependency.
Boxes carry the status glyph and colour of the stage list; the line under the graph describes the stage under the cursor.
The cursor is shared with the pipeline view (the same stage stays selected), and a graph larger than the screen scrolls to keep it in view.

## Runs outlive the interface

`dvc` runs started from the interface are detached: quitting with `q` does not stop them, and a message tells you which command
is still going. Start `dvcat` in the same project again and the run is shown with its output so far and keeps streaming
(its time counts from the real start; `x` still cancels it). A run that finished while the interface was closed is shown once,
with its log, outcome and duration. To stop a run, press `x` first. The state lives in `.dvc/tmp/dvcat-run.*` (`.dvcat/` if there is no `.dvc`).

A `dvc` process that dvcat did not start (another terminal, another tool) is noticed every couple of seconds from
`.dvc/tmp/rwlock`: the header names it (`⚠ outside dvcat: dvc repro pdf2md · 44m`), and stages it works on get a
`⚠ running outside dvcat` marker (matched by the stage named on its command line, or by outputs it holds a write lock on).
Press `X` to stop it: after a `y` it gets SIGTERM, together with what it started, and SIGKILL after 5 seconds.
A run of yours that fails on the lock opens the same question by itself.
Pressing `r`/`R` on a stage that would clash with an outside run (it reads what the other writes, or writes what the other reads or writes) is refused with the reason, e.g. `it reads data/md_files, which `dvc repro pdf2md` (PID 3134785) is writing`. Its output cannot be followed, because dvcat has no access to it.

The hidden params are remembered per project (`.dvc/tmp/dvcat-history.json`), and the info line under the table says how many are hidden.

## Run times

The log title shows how long a run has taken (live while it runs, fixed afterwards), and a stage's detail pane has a
`last run` line with the outcome and duration of its latest run from this session. DVC itself keeps no durations, so
runs made outside the interface, or before it was opened, have none. The times are also in the stage list (right-aligned)
and, for experiments run from the form, in a `run time` column next to the name. They are remembered between sessions in
`.dvc/tmp/dvcat-history.json` (inside DVC's own, Git-ignored tmp directory).
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
DVC=/path/to/dvc tests/fixtures/demo/make-git-demo.sh /tmp/dvcat-git-demo
cargo run -- /tmp/dvcat-git-demo
```

## Develop

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/dvc_cross_check.rs` compares our stale detection with the real `dvc status --json`.
It needs DVC: `DVC_BIN=/path/to/dvc cargo test --test dvc_cross_check`;
`tests/git_demo.rs` builds the Git demo and reads its experiments the same way
(without DVC it prints `SKIPPED` and passes).

## License

MIT, see `LICENSE`.

## Plots

The plots view (`4` or `p`) draws the `plots:` section of `dvc.yaml` in the terminal, reading the data files from the
workspace (CSV, TSV, JSON, YAML). Nothing is saved by dvcat: the definitions live in `dvc.yaml` and the data in your
tracked files, as in DVC. `j`/`k` pick a plot, `F5` re-reads the files, `d` shows the selected plot's definition
from `dvc.yaml` under the chart, and `n` opens a form (data file, x, y, template, title; it lists the file's columns)
(`Ctrl+F` on the file field picks the file with [`fzf`](https://github.com/junegunn/fzf), if installed)
that adds a new plot to the `plots:` section of `dvc.yaml`, leaving the rest of the file untouched.

Supported: `x`, `y` (a field, a list, or `file: fields`), `title`, `x_label`, `y_label` and the templates `linear`,
`simple`, `scatter`, `smooth` (drawn as a plain line), `bar_horizontal`, `bar_horizontal_sorted`, `confusion` and
`confusion_normalized`. Images cannot be shown, and revisions are not compared yet (`dvc plots diff`).

### Which metric is "best"?

The crown also appears in the table, in front of the best cell of each metric column. It needs to know which way a metric improves, and DVC does not say. dvcat reads it from the name: `loss`, `error`, `mae`, `mse`, `rmse`, `crps`, `rhat`, `divergences`, `nll`, `bias`, ... improve downwards; `auc`, `accuracy`, `f1`, `precision`, `recall`, `r2`, `elpd`, `ess`, `score`, `skill` (so `mae_skill` improves upwards) improve upwards. A metric whose name tells nothing (`coverage_50`: the aim is a value) never gets a crown.
