# Spec Delta

## Purpose

Let users browse the stages of a DVC project as a dependency-ordered list, with a detail pane describing what each stage runs, consumes and produces.

## ADDED Requirements

### Requirement: Load a DVC project
The system SHALL accept a project directory as a command-line argument, defaulting to the current directory, and load the stages declared in its `dvc.yaml`.

#### Scenario: Valid project
- **WHEN** the user starts the application in a directory containing a `dvc.yaml`
- **THEN** the pipeline view lists every stage declared in that file

#### Scenario: Not a DVC project
- **WHEN** the given directory has no `dvc.yaml`
- **THEN** the application exits with a message naming the directory and stating that no `dvc.yaml` was found

#### Scenario: Malformed dvc.yaml
- **WHEN** `dvc.yaml` cannot be parsed
- **THEN** the application exits with an error that includes the file path and the parse failure reason

### Requirement: Dependency-ordered stage list
The system SHALL list stages in topological order, where a stage depends on another when one of its `deps` equals or lies inside an `outs` path of the other.

#### Scenario: Upstream before downstream
- **WHEN** stage `train` depends on an output of stage `features`
- **THEN** `features` appears before `train` in the list

#### Scenario: Dependencies shown
- **WHEN** a stage is selected
- **THEN** the names of the stages it depends on are shown

#### Scenario: Cyclic definition
- **WHEN** stage dependencies form a cycle
- **THEN** the application reports the stages involved instead of crashing

### Requirement: Stage detail pane
The system SHALL show, for the selected stage, its command, deps, params and outs.

#### Scenario: Selection changes
- **WHEN** the user moves the selection with the arrow or `j`/`k` keys
- **THEN** the detail pane updates to the newly selected stage

#### Scenario: Params listed
- **WHEN** a stage declares params
- **THEN** each param key is shown with its current workspace value

### Requirement: Files pane
The system SHALL list the files of the selected stage (deps, param files, outs and metrics, each path once) in a pane of its own, and SHALL let the user move a cursor over them when the pane has focus.

#### Scenario: Files follow the selection
- **WHEN** the user selects another stage
- **THEN** the files pane lists that stage's files and the cursor returns to the first entry

#### Scenario: Focus switch
- **WHEN** the user presses `Tab`
- **THEN** focus moves between the stage list and the files pane, and only the focused pane shows a cursor

### Requirement: File preview and editor
The system SHALL open a scrollable read-only preview of the file under the cursor on `Enter`, and SHALL hand the file to `$VISUAL`/`$EDITOR` on `o`, restoring the UI afterwards.

#### Scenario: Preview a file
- **WHEN** the user presses `Enter` on a text file
- **THEN** a popup shows its first 64 KiB with line numbers, and `q` or `Esc` closes it

#### Scenario: Preview of a directory, binary or missing file
- **WHEN** the file is a directory, binary, or does not exist
- **THEN** the popup lists the directory entries, states that the file is binary, or states that it does not exist yet, instead of failing

#### Scenario: Open in editor
- **WHEN** the user presses `o` on an existing file
- **THEN** the editor takes over the terminal, and on exit the UI returns and stage statuses are rechecked

#### Scenario: Editor for a missing file
- **WHEN** the user presses `o` on a file that does not exist
- **THEN** a notice says so and no editor is started

### Requirement: Visual theme
The system SHALL draw with the Catppuccin Mocha palette: a filled background, rounded pane borders, a lit border on the focused pane, a header with the project name and view tabs, and key hints in the footer.

#### Scenario: Focused pane
- **WHEN** a pane has focus
- **THEN** its border and title are drawn in the accent colour and other panes in a muted one

### Requirement: Quit and help
The system SHALL exit and restore the terminal when the user presses `q`, and SHALL show the available key bindings on screen.

#### Scenario: Quit restores terminal
- **WHEN** the user presses `q`
- **THEN** the terminal returns to its normal mode with the prior screen content intact

#### Scenario: Terminal restored after failure
- **WHEN** the application terminates because of an error
- **THEN** the terminal is restored before the error message is printed
