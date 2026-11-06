# Spec Delta

## Purpose

Let users run a selected stage from the interface with `dvc repro`, watch its output live without freezing the UI, and cancel it.

## ADDED Requirements

### Requirement: Run the selected stage
The system SHALL run only the selected stage (`dvc repro --single-item <stage>`) when the user presses `r`, and SHALL run it together with its stale upstream stages (`dvc repro <stage>`) when the user presses `R`.

#### Scenario: Run one stage
- **WHEN** the user presses `r` on stage `train`
- **THEN** `dvc repro --single-item train` is started in the project directory

#### Scenario: Run with upstream
- **WHEN** the user presses `R` on stage `train`
- **THEN** `dvc repro train` is started so that stale upstream stages run first

#### Scenario: Run already in progress
- **WHEN** a run is active and the user presses `r` or `R`
- **THEN** no second process is started and the interface says a run is in progress

### Requirement: Live log
The system SHALL display the run's combined stdout and stderr in a log pane as it is produced, and the interface SHALL remain responsive to input while the run is active.

#### Scenario: Streaming output
- **WHEN** the running command prints lines over time
- **THEN** each line appears in the log pane without waiting for the process to finish

#### Scenario: Carriage-return progress
- **WHEN** the command rewrites a line using carriage returns
- **THEN** the log shows the latest state of that line instead of every intermediate redraw

#### Scenario: Navigation during run
- **WHEN** a run is active
- **THEN** the user can still move the selection and scroll the log

### Requirement: Run outcome
The system SHALL show whether the run succeeded or failed with its exit code, and SHALL refresh stage statuses afterward.

#### Scenario: Success
- **WHEN** the process exits with code 0
- **THEN** the run is shown as succeeded and stage statuses are recomputed from the updated `dvc.lock`

#### Scenario: Failure
- **WHEN** the process exits with a non-zero code
- **THEN** the run is shown as failed with that code and the log remains visible

#### Scenario: DVC not found
- **WHEN** no `dvc` executable is found on `PATH` or in the project's `.venv/bin`
- **THEN** the run does not start and the interface states where it looked

### Requirement: Cancel a run
The system SHALL stop an active run when the user presses `x`, without leaving orphaned child processes.

#### Scenario: Cancel
- **WHEN** a run is active and the user cancels it
- **THEN** the process is terminated, the log shows the run as cancelled, and a new run can then be started

#### Scenario: Quit during run
- **WHEN** the user quits while a run is active
- **THEN** the process is terminated before the application exits

### Requirement: Run duration
The system SHALL show how long a run has taken, live while it is active and fixed once it ends, and SHALL remember how the latest run of each stage started from the interface ended and how long it took.

#### Scenario: Live and final time
- **WHEN** a run is active, and then ends
- **THEN** the log title shows the elapsed time, which keeps growing while the run is active and stays at the final duration afterwards

#### Scenario: Last run of a stage
- **WHEN** a stage run has finished in this session
- **THEN** the stage's detail pane shows its outcome and duration, and a later run of the same stage replaces them

#### Scenario: Nothing to report
- **WHEN** a stage has not been run from the interface, or its run never started
- **THEN** no last-run line is shown for it

#### Scenario: Skipped stage
- **WHEN** DVC skips the requested stage because nothing changed
- **THEN** that run is not recorded as the stage's last run, and an earlier record is kept; a skipped upstream stage does not affect the requested stage's record

