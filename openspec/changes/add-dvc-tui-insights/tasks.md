# Tasks

## 1. foreach / matrix / vars expansion

- [x] 1.1 Add the `expand` module: collect `vars` (inline mappings, files) and default `params.yaml`; substitute `${name}`, `${item}`, `${key}`, `${item.field}`; verify unit tests including type-preserving whole-string references and unresolved references
- [x] 1.2 Expand `foreach` (list of scalars, list of mappings, mapping, and `foreach: ${var}`) and `matrix` with DVC's stage names; verify unit tests for each naming rule against the names in the design
- [x] 1.3 Feed expanded stages into `Project::load`, flagging only unresolved stages as unsupported; verify existing tests still pass and a new fixture project shows expanded stages ordered with correct edges in a `TestBackend` test
- [x] 1.4 Cross-check with real DVC: run `dvc repro` on a templated fixture and compare our stage names, and our stale verdicts after editing one item's input, with `dvc.lock` / `dvc status --json`; run one expanded stage (`name@us`) from the UI and watch it turn up to date
- [x] 1.5 Update README (what is expanded, what is flagged) and the fixture; verify `cargo test` and clippy pass

## 2. Output check

- [x] 2.1 Add `OutMissing`, `OutModified` and `OutUnverified` reasons and the size-then-md5 comparison with the 256 MiB limit; verify unit tests for missing, edited (same size and different size), directory, no-hash and over-limit outputs
- [x] 2.2 Show the new reasons in the detail pane and keep downstream propagation; verify `TestBackend` tests on the demo fixture with a deleted and an edited `model.txt`
- [x] 2.3 Extend `tests/dvc_cross_check.rs` with deleted and edited output scenarios; verify the stage sets agree with `dvc status --json`
- [x] 2.4 Manual pass on the Git demo: delete `model.txt`, see `train` stale with "missing output", run it, see it return to up to date; update README

## 3. Metrics diff

- [x] 3.1 Add baseline lookup, changed-param detection and delta formatting (precision rule, sign, direction) as pure functions; verify unit tests including float noise, missing values and the baseline/workspace rows
- [x] 3.2 Highlight changed param cells and add the `m` delta mode to the experiments view with neutral direction colours; verify `TestBackend` tests on the captured `two.json` sample and that sorting still uses underlying values
- [x] 3.3 Manual pass on the Git demo against real `exp show`; update README keys

## 4. Metrics plot

- [x] 4.1 Add the pure `plot` points function (param on X, creation order otherwise, metric choice, non-numeric and missing values skipped); verify unit tests
- [x] 4.2 Add the `p` panel with `Chart`, metric cycling (`[`/`]`), cursor point and the "nothing to plot" state; verify `TestBackend` tests, including tiny terminals
- [x] 4.3 Manual pass on the Git demo (lr against auc); update README keys

## 5. Experiment branch

- [x] 5.1 Replace `Run.is_experiment` by a `RunKind`, keeping table reload for experiment, apply and remove runs; verify all existing run tests pass unchanged in behaviour
- [x] 5.2 Add the `Prompt` modal, branch-name validation and `runner::exp_branch_args`; verify unit tests for each invalid-name rule and for default, edited and cancelled prompts
- [x] 5.3 Wire `b`, the success notice (`git switch <name>`) and the failure display; verify fake-`dvc` tests, then a real `exp branch` in the Git demo checked with `git branch`
- [x] 5.4 Update README keys and spec checklist; verify clippy and tests

## 6. DAG view

- [ ] 6.1 Add layer assignment and ordering (barycenter sweep) with virtual nodes for layer-skipping edges; verify unit tests on chain, fork, join, diamond and skip-layer graphs
- [ ] 6.2 Add the direction-bit canvas, track allocation and box/arrow drawing; verify golden-string tests for the same graphs, including crossings
- [ ] 6.3 Add the `g` view: status colours, cursor navigation along edges and layers, scrolling, Enter to jump, `r`/`R` to run, empty-project state; verify `App` key tests and `TestBackend` tests
- [ ] 6.4 Manual pass on the demo and on `articleIVdb` (read-only) at several terminal sizes; update README keys and legend

## 7. Integration and wrap-up

- [ ] 7.1 End-to-end pass on the Git demo and a templated fixture covering every scenario in the six specs by checklist; fix gaps found
- [ ] 7.2 Run `openspec validate add-dvc-tui-insights --strict`, `cargo fmt`, clippy with `-D warnings` and all tests including the DVC-backed ones
- [ ] 7.3 Inspect `jj status` and `jj diff --git`, describe the revision and move the task bookmark; verify `jj log`

## Workflow follow-up

- Archive `add-dvc-tui` first if its specs should become the base that these capabilities extend, then archive this change after review.
