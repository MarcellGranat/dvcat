# Spec Delta

## Purpose

Make execution times visible where the stages and experiments are listed, and keep them between sessions, because DVC records none.

## ADDED Requirements

### Requirement: Time in the stage list
The system SHALL show, right-aligned in each stage's row of the stage list, how long that stage's latest finished run from this interface took, and SHALL show nothing for stages without one.

#### Scenario: Stage with a recorded run
- **WHEN** a stage has a recorded latest run
- **THEN** its row ends with the duration (for example `3.2s`), and other rows show no time

#### Scenario: Narrow list
- **WHEN** the list is too narrow for the name and the time
- **THEN** the time is left out rather than overlapping the name

#### Scenario: Skipped runs
- **WHEN** DVC skipped the stage because nothing changed
- **THEN** that run does not change the stage's shown time

### Requirement: Time in the experiments table
The system SHALL show a "run time" column, next to the name, with the duration of each experiment that was run from this interface, once at least one experiment has a known duration.

#### Scenario: Known and unknown experiments
- **WHEN** one experiment was run from the interface and others were not
- **THEN** only that experiment's row has a time, and the baseline and workspace rows are empty

#### Scenario: No known times
- **WHEN** no experiment has a known duration
- **THEN** the column is not shown

#### Scenario: Naming
- **WHEN** an experiment run finishes successfully
- **THEN** its duration is stored under the experiment name that DVC reports for the run, including names DVC generated itself

#### Scenario: Sorting
- **WHEN** the table is sorted by the run time column
- **THEN** durations compare as numbers (9 seconds before 2 minutes) and rows without a time come last in either direction

#### Scenario: Not for other actions
- **WHEN** an experiment is applied, removed, or promoted to a branch, or a run fails
- **THEN** no duration is stored for it

### Requirement: Times survive restarts
The system SHALL remember recorded durations between sessions in the project's `.dvc/tmp` directory, and SHALL work normally when that cannot be read or written.

#### Scenario: Reopening
- **WHEN** the interface is closed and opened again on the same project
- **THEN** the stage list and the experiments table show the durations recorded earlier

#### Scenario: Unwritable or absent history
- **WHEN** the project has no `.dvc` directory, or the history file is missing, damaged or from a newer version
- **THEN** no error is shown, durations simply start empty, and nothing is created outside `.dvc/tmp`
