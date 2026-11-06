# Spec Delta

## Purpose

Let users compare DVC experiments side by side, showing each experiment's params and metrics in a sortable table.

## ADDED Requirements

### Requirement: Experiments table
The system SHALL show a table of experiments built from `dvc exp show --json`, with one row per experiment, the workspace row first, and columns for name, params and metrics.

#### Scenario: Experiments present
- **WHEN** the user opens the experiments view and the project has experiments
- **THEN** each experiment appears as a row with its name and its param and metric values

#### Scenario: Workspace row
- **WHEN** the table is shown
- **THEN** the first row is the current workspace, distinguished from committed and experiment rows

#### Scenario: No experiments
- **WHEN** the project has only the workspace and baseline revisions
- **THEN** the table shows those rows and a hint on how to create an experiment

### Requirement: Param and metric columns
The system SHALL derive columns from the union of param and metric keys across rows, prefix each key with its file name, and leave cells empty where a row lacks the key.

#### Scenario: Nested params
- **WHEN** `params.json` contains `download.page_size`
- **THEN** a column labelled `params.json:download.page_size` is shown

#### Scenario: No metrics defined
- **WHEN** no row has any metric
- **THEN** no metric columns are shown and the view does not fail

### Requirement: Sorting
The system SHALL let the user choose a column and toggle ascending or descending order, sorting numbers numerically and other values as text, with empty cells last.

#### Scenario: Sort by metric
- **WHEN** the user sorts by a numeric metric column descending
- **THEN** rows are ordered by that value from highest to lowest, with rows lacking a value at the end

### Requirement: Loading and errors
The system SHALL fetch experiments without blocking the interface, and SHALL show a readable message when the fetch fails.

#### Scenario: Slow fetch
- **WHEN** `dvc exp show` takes noticeable time
- **THEN** a loading indicator is shown and the other views stay usable

#### Scenario: Command failure
- **WHEN** `dvc exp show` exits with an error or its output is not valid JSON
- **THEN** the view shows the error text instead of a table

#### Scenario: Refresh
- **WHEN** the user presses `F5` in the experiments view
- **THEN** the experiments are fetched again
