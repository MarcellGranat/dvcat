# Tasks

Implementation note: the author is learning Rust. While applying these tasks, explain each new concept as it appears (use the `rust-teacher` skill) and ask for confirmation before larger edits. Run `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` at the end of every group.

## 1. Project skeleton and test fixture

- [x] 1.1 Create the cargo binary `dvc_tui` with the dependencies from design decision 9; verify `cargo build` succeeds and `cargo run` prints a placeholder
- [x] 1.2 Create `tests/fixtures/demo/`, a throwaway three-stage DVC repo (`prepare` → `features` → `train`) with `params.yaml` and a metrics file, plus a script that regenerates its `dvc.lock`; verify `dvc status` and `dvc repro` run cleanly inside it
- [x] 1.3 Add a minimal `README.md` with run instructions and key bindings; verify the documented commands work as written

## 2. Terminal loop and event plumbing

- [x] 2.1 Implement terminal setup/teardown with a panic hook and drop guard; verify via a test or manual check that the terminal is restored after `q` and after a forced panic (spec: pipeline-view, quit and help)
- [x] 2.2 Implement the `Event` enum, `mpsc` channel, key-forwarding thread and `recv_timeout` main loop with `update`/`view` split; verify `q` quits and the screen shows the key-binding help line
- [x] 2.3 Unit-test `update` for key events (quit, selection movement) without a terminal; verify `cargo test` passes

## 3. Loading the pipeline

- [x] 3.1 Implement serde types and loaders for `dvc.yaml`, `dvc.lock` and param files (JSON, YAML); verify unit tests parse the fixture and a copy of the `articleIVdb` files
- [x] 3.2 Implement CLI arg handling (project path, default `.`) and the error cases: missing `dvc.yaml`, malformed YAML with file path in the message; verify with tests for both errors
- [x] 3.3 Implement graph building (outs → deps path-prefix matching), Kahn topological sort and cycle reporting; verify tests cover ordering, nested paths and a cycle

## 4. Pipeline view

- [x] 4.1 Render the stage list with the dependency names of the selected stage; verify with a ratatui `TestBackend` snapshot of the fixture
- [x] 4.2 Render the detail pane (cmd, deps, params with current values, outs) and update on selection change; verify a `TestBackend` test for a stage with params
- [x] 4.3 Run the app against `~/projects/articleIVdb` read-only and the fixture; verify both display correctly by hand

## 5. Stale detection

- [x] 5.1 Implement `Reason` and `stale_reasons` for never-run, cmd changed and param changed (including removed keys); verify unit tests, including the `download.limit: 3 → null` case
- [x] 5.2 Add md5 dependency checking (changed and missing files) and `DirUnverified` for directory deps; verify tests with temp files
- [x] 5.3 Add upstream propagation over topological order and the missing-`dvc.lock` case; verify tests
- [x] 5.4 Compute statuses off the UI thread, show status glyphs in the list and reasons in the detail pane; verify with a `TestBackend` test and a manual run on `articleIVdb` showing `download` as stale
- [x] 5.5 Cross-check against real DVC: a test or script comparing our verdict with `dvc status --json` on the fixture after editing a param, a dep and the cmd; verify the verdicts agree

## 6. Stage runs

- [x] 6.1 Implement `dvc` lookup (env var, project `.venv/bin`, `PATH`) with an error listing the searched places; verify unit tests
- [x] 6.2 Implement spawning in its own process group, merged stdout/stderr readers, `\n`/`\r` line handling and ANSI stripping, sending `Event`s; verify tests against a small shell script that prints progress with `\r`
- [x] 6.3 Wire `r` (`--single-item`), `R` and the single-run rule into `update`, with a log pane showing live output; verify by running a fixture stage and watching output stream while navigation stays responsive
- [x] 6.4 Show success/failure with exit code, recompute stage statuses after the run, and implement `x` cancel plus terminate-on-quit with TERM-then-KILL escalation; verify with a long-running script that no child process remains after cancel or quit

## 7. Experiments table

- [x] 7.1 Implement serde types for `exp show --json` and the pure table model (column union, `file:key` labels, empty cells); verify unit tests on a captured sample from `articleIVdb` and one with metrics from the fixture
- [x] 7.2 Implement sorting (numeric vs text, empty last, asc/desc toggle); verify unit tests
- [x] 7.3 Add the experiments view with background fetch, loading indicator, error display, `F5` refresh, and disable-during-run message; verify with `TestBackend` tests and a manual run

## 8. Look, files pane and Git demo

- [x] 8.1 Add `theme.rs` (Catppuccin Mocha), rounded panes, focus styling, header with view tabs and footer key hints; verify with `TestBackend` tests on colours and layout
- [x] 8.2 Add the files pane (`Tab` focus, cursor, `Enter` preview popup, bounded read, directory/binary/missing handling); verify unit tests and a manual run
- [x] 8.3 Add `o` to open a file in `$VISUAL`/`$EDITOR` with terminal suspend/resume and a pausable key thread; verify manually with a real editor
- [x] 8.4 Add `tests/fixtures/demo/make-git-demo.sh` (Git copy with three experiments) and `tests/git_demo.rs`; verify against real DVC

## 9. Integration and wrap-up

- [ ] 9.1 End-to-end manual pass on the fixture: browse, see stale, run `R` on `train`, see statuses turn green, open experiments and sort by a metric; verify every spec scenario for the four capabilities by checklist
- [ ] 9.2 Run `openspec validate add-dvc-tui --strict`; verify it passes
- [ ] 9.3 Inspect `jj status` and `jj diff --git`, describe the revision and move the task bookmark as in the workflow rules; verify `jj log` shows the described revision

## Workflow follow-up

- Archive the change after implementation and review are complete.
