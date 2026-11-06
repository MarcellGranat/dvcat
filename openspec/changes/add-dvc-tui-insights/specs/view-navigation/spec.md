# Spec Delta

## Purpose

Make the three views (pipeline, graph, experiments) discoverable and reachable directly.

## ADDED Requirements

### Requirement: Numbered views
The system SHALL label the views in the header with the number key that opens them and, for the graph and experiments, their letter key, and SHALL switch to a view directly when its number is pressed.

#### Scenario: Header labels
- **WHEN** any view is shown
- **THEN** the header lists `1 Pipeline`, `2 Graph (g)` and `3 Experiments (e)`, with the current view highlighted

#### Scenario: Direct switch
- **WHEN** the user presses `1`, `2` or `3` outside a popup
- **THEN** the pipeline, graph or experiments view opens (loading the experiments on first use)

#### Scenario: Typing in a popup
- **WHEN** a popup or form is open and the user types a digit
- **THEN** the digit goes to the popup and the view does not change

### Requirement: Footer mentions the graph
The system SHALL list the graph key among the pipeline and files-pane key hints.

#### Scenario: Pipeline footer
- **WHEN** the pipeline view is shown
- **THEN** the footer includes `g graph`
