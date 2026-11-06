# Spec Delta

## Purpose

Treat `foreach`, `matrix` and `vars` in `dvc.yaml` the way DVC does, so templated pipelines are listed, ordered, checked and run like any other.

## ADDED Requirements

### Requirement: foreach expansion
The system SHALL expand a `foreach` stage over a list or a mapping into one stage per item, substituting `${item}`, `${key}` and `${item.<field>}` in the `do` template. Stages are named as DVC names them: `<stage>@<value>` for a list of scalars, `<stage>@<index>` for a list of mappings, and `<stage>@<key>` for a mapping.

#### Scenario: List of values
- **WHEN** `foreach: [us, eu]` with `do: {cmd: "run ${item}", outs: ["out/${item}.csv"]}`
- **THEN** stages `name@us` and `name@eu` exist with commands `run us` and `run eu` and the substituted outputs

#### Scenario: Mapping
- **WHEN** `foreach` is a mapping whose values are mappings
- **THEN** one stage per key is produced, `${key}` is the key and `${item.<field>}` reads the value's fields

#### Scenario: List of mappings
- **WHEN** `foreach` is a list of mappings
- **THEN** the stages are named `name@0`, `name@1`, ... and `${item.<field>}` reads each mapping's fields

### Requirement: matrix expansion
The system SHALL expand a `matrix` stage into one stage per combination of its variable values, substituting `${item.<variable>}` in the stage definition.

#### Scenario: Two variables
- **WHEN** `matrix: {os: [lin, win], ver: [1, 2]}`
- **THEN** four stages exist, named `name@lin-1`, `name@lin-2`, `name@win-1` and `name@win-2`, each with `${item.os}` and `${item.ver}` substituted

### Requirement: Variables
The system SHALL resolve `${name}` references against the `vars` section (inline mappings and listed files) and the default `params.yaml`.

#### Scenario: Inline variable
- **WHEN** `vars: [{region: eu}]` and a command uses `${region}`
- **THEN** the command shows `eu`

#### Scenario: Unresolvable reference
- **WHEN** a reference cannot be resolved or uses a feature the system does not implement
- **THEN** the affected stage stays listed, flagged as unsupported with the reason, and the rest of the project loads normally

### Requirement: Expanded stages behave like any stage
The system SHALL give expanded stages the same ordering, edges, stale detection, status and run behaviour as stages written out by hand, using the expanded names that `dvc.lock` uses.

#### Scenario: Ordering and edges
- **WHEN** a stage depends on an output produced by one expanded stage
- **THEN** it is ordered after it and the edge appears in the graph

#### Scenario: Stale detection
- **WHEN** one expanded stage's input changes
- **THEN** only that stage (and what depends on it) is stale

#### Scenario: Running
- **WHEN** the user runs `name@us`
- **THEN** the system runs `dvc repro` for exactly that stage name

#### Scenario: Agreement with DVC
- **WHEN** the project is compared with `dvc status --json`
- **THEN** the stages named in the output are the expanded names listed by the system
