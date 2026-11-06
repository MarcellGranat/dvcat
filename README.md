# dvc_tui

A minimal terminal UI for [DVC](https://dvc.org): browse a pipeline, see which stages are stale and why,
run a stage with a live log, and compare experiments. Written in Rust with [ratatui](https://ratatui.rs).

> **Status:** under construction. The change plan lives in `openspec/changes/add-dvc-tui/`.
> Features below are marked *(planned)* until they are implemented.
> Working today: pipeline list, detail pane, stale detection with reasons, running and cancelling stages.

## Run

```sh
cargo run -- [PROJECT_DIR]      # PROJECT_DIR defaults to the current directory
```

`dvc` is looked up in `$DVC_TUI_DVC`, then `<PROJECT_DIR>/.venv/bin/dvc`, then `PATH`.
If none is found, pressing `r` explains where it looked.

## Keys

| Key        | Action                                   |
|------------|------------------------------------------|
| `j` / `k`  | move selection down / up                 |
| `q`        | quit                                     |
| `r`        | run the selected stage only (`dvc repro --single-item`) |
| `R`        | run it with stale upstream stages (`dvc repro`) |
| `x`        | cancel the running stage                 |
| `PgUp` / `PgDn` | scroll the log                      |
| `e` *(planned)* | experiments view                    |
| `F5` *(planned)* | refresh experiments                |

## Status glyphs

`✓` up to date · `●` stale (the detail pane says why) · `○` never run · `?` unverified (e.g. a directory dependency) · `…` still checking.

## Demo project

`tests/fixtures/demo` is a throwaway three-stage DVC project (`prepare` → `features` → `train`).
Rebuild its DVC state (needs `dvc` on `PATH`, or set `DVC=/path/to/dvc`):

```sh
tests/fixtures/demo/regen.sh
```

## Develop

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/dvc_cross_check.rs` compares our stale detection with the real `dvc status --json`.
It needs DVC: `DVC_BIN=/path/to/dvc cargo test --test dvc_cross_check`
(without DVC it prints `SKIPPED` and passes).
