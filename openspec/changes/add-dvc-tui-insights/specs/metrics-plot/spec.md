# Spec Delta

## Purpose

Chart one metric across the experiments in the table so trends (for example metric against learning rate) are visible without leaving the terminal.

## ADDED Requirements

### Requirement: Plot panel
The system SHALL show, on request, a chart below the experiments table with one metric on the vertical axis and one point per experiment.

#### Scenario: Plot toggled
- **WHEN** the user presses `p` in the experiments view
- **THEN** a chart appears (and a second press hides it) without discarding the table cursor or sort

#### Scenario: No numeric data
- **WHEN** no metric column has numeric values for at least two experiments
- **THEN** the panel says there is nothing to plot instead of drawing an empty chart

### Requirement: Axes
The system SHALL use the numeric param column under the cursor as the horizontal axis, and the creation order of the experiments otherwise, and SHALL let the user choose which metric is plotted.

#### Scenario: Param on the X axis
- **WHEN** the cursor is on a numeric param column
- **THEN** each experiment is plotted at its param value, and the axis is labelled with that column

#### Scenario: Cursor elsewhere
- **WHEN** the cursor is on the name column or a metric column
- **THEN** experiments are plotted in creation order

#### Scenario: Choosing the metric
- **WHEN** the user presses the metric-cycle keys
- **THEN** the next or previous metric column is plotted and its name is shown

### Requirement: Selection is visible
The system SHALL mark the point of the experiment under the table cursor.

#### Scenario: Cursor point
- **WHEN** the table cursor moves to another experiment
- **THEN** the highlighted point moves with it
