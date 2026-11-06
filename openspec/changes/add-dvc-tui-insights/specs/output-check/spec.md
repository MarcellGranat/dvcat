# Spec Delta

## Purpose

Notice when a stage's outputs no longer match what `dvc.lock` recorded, so a deleted or hand-edited output is not reported as up to date.

## ADDED Requirements

### Requirement: Output comparison
The system SHALL compare each output a stage recorded in `dvc.lock` with the workspace and mark the stage stale when an output is missing or its content differs from the recorded one.

#### Scenario: Output deleted
- **WHEN** a file output listed in `dvc.lock` does not exist in the workspace
- **THEN** the stage is stale and that path is listed as a missing output

#### Scenario: Output edited
- **WHEN** a file output exists but its content differs from the hash recorded in `dvc.lock`
- **THEN** the stage is stale and that path is listed as a modified output

#### Scenario: Output unchanged
- **WHEN** every recorded output exists with the recorded content
- **THEN** outputs add no reason to the stage's status

#### Scenario: Agreement with DVC
- **WHEN** an output is deleted or edited in a project whose other inputs are unchanged
- **THEN** the set of stages the system marks stale for their own reasons equals the set `dvc status --json` reports

### Requirement: Cheap checks first
The system SHALL decide most outputs without hashing them fully and SHALL NOT block the interface while checking.

#### Scenario: Size differs
- **WHEN** an output's size differs from the size recorded in `dvc.lock`
- **THEN** it is reported as modified without computing its hash

#### Scenario: Very large output
- **WHEN** an output with a matching size is larger than the hashing limit
- **THEN** it is reported as unverified instead of being hashed, and the stage is not shown as up to date solely on that basis

### Requirement: Directory and special outputs
The system SHALL treat directory outputs as unverified, and SHALL skip outputs that `dvc.lock` records without a hash.

#### Scenario: Directory output
- **WHEN** a stage's recorded output is a directory
- **THEN** the output is flagged unverified and no false "modified" or "missing" verdict is given for its contents
