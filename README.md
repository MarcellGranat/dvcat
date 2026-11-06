# dvc_tui

A minimal terminal UI for [DVC](https://dvc.org): browse a pipeline, see which stages are stale and why,
run a stage with a live log, and compare experiments. Written in Rust with [ratatui](https://ratatui.rs).

> **Status:** under construction. The change plan lives in `openspec/changes/add-dvc-tui/`.
> Features below are marked *(planned)* until they are implemented.

## Run

```sh
cargo run -- [PROJECT_DIR]      # PROJECT_DIR defaults to the current directory
```

`dvc` is looked up in `$DVC_TUI_DVC`, then `<PROJECT_DIR>/.venv/bin/dvc`, then `PATH`.

## Keys *(planned)*

| Key        | Action                                   |
|------------|------------------------------------------|
| `j` / `k`  | move selection down / up                 |
| `r`        | run selected stage only (`dvc repro --single-item`) |
| `R`        | run selected stage with stale upstream (`dvc repro`) |
| `x`        | cancel the running stage                 |
| `e`        | experiments view                         |
| `F5`       | refresh experiments                      |
| `q`        | quit                                     |

## Demo project

`tests/fixtures/demo` is a throwaway three-stage DVC project (`prepare` → `features` → `train`).
Rebuild its DVC state (needs `dvc` on `PATH`, or set `DVC=/path/to/dvc`):

```sh
tests/fixtures/demo/regen.sh
```

## Develop

```sh
cargo fmt && cargo clippy -- -D warnings && cargo test
```
