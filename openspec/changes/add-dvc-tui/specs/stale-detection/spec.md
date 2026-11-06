# Spec Delta

## Purpose

Tell users which stages are out of date and why, by comparing the state recorded in `dvc.lock` with the current workspace, without running a stage.

## ADDED Requirements

### Requirement: Stage status indicator
The system SHALL show one of four statuses for each stage in the list: up to date, stale, never run, or unverified (some input could not be checked).

#### Scenario: Never run
- **WHEN** a stage has no entry in `dvc.lock`
- **THEN** it is shown as never run

#### Scenario: Up to date
- **WHEN** a stage's locked params, dep hashes and cmd all match the workspace
- **THEN** it is shown as up to date

#### Scenario: Unverified
- **WHEN** a stage has no detected change but at least one dependency could not be verified
- **THEN** it is shown as unverified, not as up to date

#### Scenario: Missing lock file
- **WHEN** the project has no `dvc.lock`
- **THEN** every stage is shown as never run

### Requirement: Param change detection
The system SHALL mark a stage stale when any param it tracks differs between `dvc.lock` and the current param file, and SHALL show each differing key with its locked and current values.

#### Scenario: Changed param value
- **WHEN** `dvc.lock` records `download.limit: 3` and `params.json` now has `download.limit: null`
- **THEN** the stage is stale and the detail pane shows `download.limit: 3 → null`

#### Scenario: Param file lacks the key
- **WHEN** a tracked param key no longer exists in the param file
- **THEN** the key is shown as removed and the stage is stale

### Requirement: Added and removed dependencies
The system SHALL mark a stage stale when its `deps` in `dvc.yaml` differ in membership from those recorded in `dvc.lock`.

#### Scenario: Dependency added
- **WHEN** `dvc.yaml` lists a dependency that `dvc.lock` does not record for the stage
- **THEN** the stage is stale and that path is listed as a new dependency

#### Scenario: Dependency removed
- **WHEN** `dvc.lock` records a dependency that `dvc.yaml` no longer lists
- **THEN** the stage is stale and that path is listed as removed

### Requirement: Dependency change detection
The system SHALL mark a stage stale when the md5 of a tracked dependency file differs from the hash in `dvc.lock`, or when the file is missing.

#### Scenario: Edited dependency
- **WHEN** a dep file's content changed since the lock was written
- **THEN** the stage is stale and that dep path is listed as changed

#### Scenario: Missing dependency
- **WHEN** a dep path does not exist in the workspace
- **THEN** the stage is stale and that dep path is listed as missing

### Requirement: Command change detection
The system SHALL mark a stage stale when its `cmd` in `dvc.yaml` differs from the `cmd` recorded in `dvc.lock`.

#### Scenario: Edited command
- **WHEN** a stage's command was changed in `dvc.yaml` after the last run
- **THEN** the stage is stale and the old and new commands are shown

### Requirement: Upstream propagation
The system SHALL mark a stage stale when any stage it depends on is stale or never run.

#### Scenario: Upstream stale
- **WHEN** stage `features` is stale and `train` depends on it
- **THEN** `train` is shown as stale with the reason "upstream: features"

### Requirement: Directory outputs and unsupported checks
For directory dependencies, the system SHALL report an unverified status rather than a possibly wrong result, and SHALL offer to defer to `dvc status --json` for an authoritative answer.

#### Scenario: Directory dependency
- **WHEN** a stage depends on a directory whose hash cannot be verified by the built-in check
- **THEN** that dependency is flagged as unverified and the stage is not shown as up to date solely on that basis
