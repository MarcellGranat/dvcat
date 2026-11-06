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

### Requirement: Quit and help
The system SHALL exit and restore the terminal when the user presses `q`, and SHALL show the available key bindings on screen.

#### Scenario: Quit restores terminal
- **WHEN** the user presses `q`
- **THEN** the terminal returns to its normal mode with the prior screen content intact

#### Scenario: Terminal restored after failure
- **WHEN** the application terminates because of an error
- **THEN** the terminal is restored before the error message is printed
