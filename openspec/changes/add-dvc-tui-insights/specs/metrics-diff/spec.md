# Spec Delta

## Purpose

Show what an experiment changed relative to its baseline, so the table answers "what did I change and what did it do" at a glance.

## ADDED Requirements

### Requirement: Changed params highlighted
The system SHALL highlight, in each experiment row, the param values that differ from the experiment's baseline.

#### Scenario: Param differs
- **WHEN** an experiment has `train.lr = 0.05` and its baseline has `0.01`
- **THEN** that cell is highlighted and the others in the row are not

#### Scenario: Baseline and workspace rows
- **WHEN** a row is the baseline or the workspace
- **THEN** it has no highlighted cells

### Requirement: Metric deltas
The system SHALL offer a mode in which numeric metric cells of experiment rows show the signed difference from the baseline instead of the absolute value, and SHALL let the user switch back.

#### Scenario: Delta mode on
- **WHEN** the user turns delta mode on and an experiment has `auc = 0.85` against a baseline `0.66`
- **THEN** the cell shows a positive difference of `0.19` with a sign, and the baseline row still shows its absolute value

#### Scenario: Non-numeric or missing values
- **WHEN** either side of a metric is missing or not a number
- **THEN** the cell shows the absolute value (or stays empty) rather than a wrong difference

#### Scenario: Sorting in delta mode
- **WHEN** the table is sorted while delta mode is on
- **THEN** rows are ordered by the underlying values, not by the displayed text

### Requirement: No judgement of better or worse
The system SHALL show the direction of a change without claiming that higher or lower is better.

#### Scenario: Direction only
- **WHEN** a metric rises or falls
- **THEN** the cell shows an up or down indication in a neutral colour scheme that is the same for every metric
