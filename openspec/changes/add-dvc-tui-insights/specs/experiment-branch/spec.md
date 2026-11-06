# Spec Delta

## Purpose

Keep a promising experiment by promoting it to a Git branch, without leaving the interface.

## ADDED Requirements

### Requirement: Branch from an experiment
The system SHALL let the user promote the experiment under the cursor to a Git branch with `dvc exp branch`, after asking for the branch name.

#### Scenario: Default name
- **WHEN** the user presses `b` on an experiment row
- **THEN** a prompt opens pre-filled with `<experiment>-branch`, and Enter runs the command with that name

#### Scenario: Custom name
- **WHEN** the user edits the name and presses Enter
- **THEN** the command runs with the edited name

#### Scenario: Cancel
- **WHEN** the user presses Esc in the prompt
- **THEN** nothing runs

### Requirement: Name validation
The system SHALL refuse branch names that Git would reject and SHALL say why.

#### Scenario: Invalid name
- **WHEN** the name is empty, contains spaces or any of `~ ^ : ? * [ \`, contains `..`, starts with `-` or `.`, or ends with `/` or `.lock`
- **THEN** the prompt stays open with a message and nothing runs

### Requirement: Outcome and next step
The system SHALL show the command's output and, on success, tell the user the branch exists but is not checked out.

#### Scenario: Success
- **WHEN** the command succeeds
- **THEN** the log shows DVC's output and a notice names the branch and says how to switch to it

#### Scenario: Branch exists or command fails
- **WHEN** the command fails (for example the branch already exists)
- **THEN** the log shows DVC's error and the project is left unchanged

### Requirement: Only experiment rows
The system SHALL offer the action only on experiment rows, while no other run is active.

#### Scenario: Not available
- **WHEN** the cursor is on the workspace or baseline row, or a run is active, or the table is not loaded
- **THEN** no prompt opens and a notice says why
