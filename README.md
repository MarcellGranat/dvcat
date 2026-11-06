# dvc_tui

A minimal terminal UI for [DVC](https://dvc.org): browse a pipeline, see which stages are stale and why,
run a stage with a live log, and compare experiments. Written in Rust with [ratatui](https://ratatui.rs).

> **Status:** under construction. The change plan lives in `openspec/changes/add-dvc-tui/`.
> Features below are marked *(planned)* until they are implemented.
> Working today: pipeline list, detail pane, files pane with preview and editor, stale detection with reasons, running and cancelling stages, an experiments table. The look is [Catppuccin Mocha](https://catppuccin.com) and needs a true-colour terminal.

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
| `h` / `l`  | *(experiments)* move the column cursor   |
| `s`        | *(experiments)* sort by the cursor column; again flips the direction |
| `F5`       | *(experiments)* reload with `dvc exp show --json` |

The files pane lists what the selected stage reads and writes (deps, param files, outs, metrics).
A preview shows at most the first 64 KiB of a file; directories are listed and binary files are not dumped.
While a preview is open it receives all keys (`q` closes it), except `Ctrl+C`, which still quits.

## Status glyphs

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
