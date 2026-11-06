# Design

## Context

Builds on the code from `add-dvc-tui` (Elm-style `App::update` returning `Effect`s, modal popups for preview, form and confirmation, a shared `Run`/`start_job` path for `dvc` processes, pure `stale::compute`, `graph::build`, `experiments::Table`). See `proposal.md` for the motivation. Facts established for this change:

- `dvc.lock` records, per output, `path`, `hash`, `md5` and `size`; a directory output has an `md5` ending in `.dir`. The size is enough to decide most "modified" cases without hashing.
- `dvc stage` naming for templated stages (checked against DVC 3.67.1 with a scratch project): `foreach` over scalars gives `name@us`, over a list of mappings `name@0`, over a mapping `name@<key>`; a `matrix` gives `name@lin-1` (values joined by `-` in declaration order). `dvc.lock` uses the expanded names, and `dvc repro name@us` runs one of them. Matrix variables are read as `${item.<var>}`.
- `dvc exp branch <experiment> [branch]` promotes an experiment to a branch without checking it out.
- Stage identity in the code is an index into `Project.stages`; everything downstream (graph, stale, UI) already works from that list, so expansion only has to produce more `Stage` values.

## Goals / Non-Goals

**Goals:**
- Each feature is a pure, testable module plus a thin UI layer, like the existing ones.
- The interface never blocks: hashing outputs stays on the status thread, `dvc exp branch` runs through the existing process path.
- Our stage names and staleness verdicts agree with DVC's (`dvc.lock`, `dvc status --json`) wherever the cross-check can see them.

**Non-Goals:**
- Telling the user which metric direction is "better" (no per-metric configuration).
- A general `${}` expression language (escapes, `${a:b}` default syntax, arithmetic). Unsupported references are flagged, as today.
- Log axes, zoom, or exporting charts; interactive graph editing or layout persistence.
- Checking out the new branch, or any other Git operation besides what `dvc exp branch` does.

## Decisions

### 1. Output check inside `stale_reasons`
New reasons `OutMissing(path)`, `OutModified(path)` and `OutUnverified(path)`. For each lock output: not found → missing; size differs → modified (no hashing); directory (`.dir` hash) → unverified; size matches and the file is above `HASH_LIMIT` (256 MiB) → unverified; otherwise md5 compared with the existing dependency hashing code. Outputs recorded without an `md5` are skipped. The check runs where dependency hashing already runs (the background status thread). Downstream propagation is unchanged: a stale stage makes dependents stale.
*Alternatives:* always hash (minutes for large models on every refresh); trust mtime (DVC keeps its own state database that we cannot read, and mtime alone gives false "up to date"); ask `dvc status --json` (slow and takes the repository lock).

### 2. Expansion is a pass over the YAML value, before typed parsing
A new `expand` module takes the parsed `dvc.yaml` value and returns concrete stage mappings. Steps: collect variables (`vars` entries that are inline mappings or file names, plus the default `params.yaml`, later entries overriding earlier ones); for each stage with `foreach`/`matrix`, produce one mapping per item with DVC's naming; substitute `${...}` in every string. A string that is exactly one reference to a list or mapping keeps its type (needed for `foreach: ${regions}` and list-valued `deps`); other references are stringified. Anything unresolved marks only that stage `unsupported` with the reason. The output feeds the existing `Stage` parser unchanged, so graph, stale, files pane and runs need no changes.
*Alternatives:* shell out to `dvc stage list`/`dvc repro --dry` for names (authoritative but needs DVC at load time, gives no `cmd`/`deps`, and slows start-up); expand lazily inside each consumer (duplicates logic). `tests/dvc_cross_check.rs` compares our expanded names with `dvc.lock` after a real `repro`.

### 3. Metrics diff computed at render time from the ordered rows
Ordered rows already place each experiment after its baseline, so an experiment's baseline is the nearest preceding baseline row. Param cells whose text differs from the baseline's are highlighted. Delta mode (`m`) replaces numeric metric cells of experiment rows by `▲ +0.1867`/`▼ -0.0213`, rounded to the larger decimal count of the two operands so float noise (`0.18669999`) never shows. Colours are direction-only and metric-independent. Sorting keeps using the underlying values (it never reads displayed text).
*Alternatives:* colouring good/bad (would need a per-metric direction, which DVC does not store); a separate diff screen (one more view for little gain).

### 4. Plot panel with ratatui `Chart`
`p` toggles a panel (about 12 rows) under the table. Points: one per experiment row that has the chosen metric. X is the cursor column if it is a numeric param, else creation order (timestamp, falling back to table order). `[`/`]` cycle the metric. The experiment under the cursor is a second dataset in an accent colour. Axis bounds are padded and labelled with min/max. The pure part (`plot::points(table, order, x_column, metric)`) returns the numbers; drawing is separate.
*Alternatives:* `Sparkline` (no x meaning, so no "metric against lr"); `BarChart` (poor for continuous x); log scale (deferred).

### 5. Text prompt and run kinds for `exp branch`
A small `Prompt` modal (title, one text field, validation error) reuses the field-editing code of the experiment form. `b` opens it with `<experiment>-branch`. Validation is a conservative subset of Git's ref rules, enough to give a message before DVC runs; Git remains the final authority and its error appears in the log. `Run` gains a `kind` (`Stage`, `Experiment`, `Branch`) replacing the `is_experiment` flag, because what happens after a run now differs: experiment, apply and remove runs reload the table; a branch run does not, but a successful one sets a notice naming the branch and saying `git switch <name>`.
*Alternatives:* reusing the experiment form (wrong fields); checking out the branch for the user (changes the working tree, out of scope).

### 6. DAG layout: layers, virtual nodes, a line-drawing canvas
`dag::layout(&Graph, labels)` assigns each stage a layer (longest path from a source), orders layers with one barycenter sweep, and replaces every edge that skips layers by a chain of one-row virtual nodes, so each drawn edge crosses exactly one gap between adjacent layers. Rows are 4 cells apart per node; a gap's vertical tracks are allocated greedily so overlapping edges get different tracks. Lines are written into a canvas whose cells hold direction bits (up/down/left/right) and are converted to `─│┌┐└┘├┤┬┴┼`, so crossings and junctions come out right; boxes and arrowheads are drawn on top. The result is `Layout { nodes, canvas, width, height }`, pure and golden-testable. The view scrolls a viewport to keep the cursor visible; navigation uses layer and row neighbours. `Enter` selects the stage and returns to the pipeline view; `r`/`R` reuse the existing run requests.
*Alternatives:* `dvc dag --dot` plus Graphviz (external dependency, not terminal-native); a plain indented tree (loses joins); `petgraph` layouts (no terminal drawing, new dependency).

### 7. Order of work
Expansion first (it changes which stages exist, so later features see them), then the output check, then the experiments features (diff, plot, branch), then the DAG, which depends on the final stage list and status colours.

## Risks / Trade-offs

- [Our expansion disagrees with DVC on an unusual template] → Flag only what we cannot resolve; cross-check names against a real `dvc.lock`; unresolved references stay "unsupported" instead of guessing.
- [Hashing outputs is slow on big projects] → Size check first, 256 MiB cap, off the UI thread; the cap result is "unverified", never a wrong "up to date".
- [Output check makes stages stale after a deliberate manual edit] → That matches `dvc status`; the reason names the file.
- [Delta rounding hides a tiny change] → Rounded to the operands' precision, so a real difference always shows at least one digit.
- [Plot is unreadable with many or identical points] → Needs at least two numeric points; points are small markers; the cursor point is distinct. Log axes deferred.
- [DAG layout gets wide or tangled] → Pan and scroll; one crossing-reduction sweep is enough for pipelines of tens of stages; a project with hundreds is out of scope.
- [`dvc exp branch` on an unnamed experiment] → The row label falls back to the short commit id, which DVC accepts; failures are shown in the log.
- [More modal screens to keep consistent] → One shared pattern (state in `App`, `Option<Modal>`, own key handler, own footer hints) and a test per modal that it swallows global keys.

## Open Questions

- Whether `p` should remember the chosen metric between visits (state only; no spec impact).
- Whether very long stage names in the DAG should be truncated with `…` or widen the layer (decide when drawing real projects).
