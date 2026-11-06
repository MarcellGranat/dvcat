# Spec Delta

## Purpose

Show the pipeline as a graph, so the shape of the dependencies is visible instead of inferred from a sorted list.

## ADDED Requirements

### Requirement: Graph drawing
The system SHALL offer a view, opened with `g`, that draws every stage as a box and every dependency as a line from the producing stage to the consuming one, with producers to the left of consumers.

#### Scenario: Chain
- **WHEN** stage `train` depends on `features` which depends on `prepare`
- **THEN** the boxes appear left to right in that order, joined by lines with arrowheads pointing right

#### Scenario: Branches and joins
- **WHEN** a stage has several upstream stages, or several stages share one upstream
- **THEN** each dependency is drawn as its own line and no line passes through a box

#### Scenario: Skipping a layer
- **WHEN** a stage depends on a stage more than one layer to its left
- **THEN** the line is routed around the boxes in between

### Requirement: Status colours
The system SHALL colour each box by the stage's status, using the same colours and glyphs as the stage list.

#### Scenario: Stale stage
- **WHEN** a stage is stale
- **THEN** its box shows the stale glyph and colour

### Requirement: Navigation
The system SHALL let the user move a cursor between boxes along the lines, scroll a graph larger than the screen, and jump to a stage.

#### Scenario: Move along edges
- **WHEN** the cursor is on a box and the user presses `l` or `h`
- **THEN** the cursor moves to a downstream or upstream stage, and `j`/`k` move between stages in the same layer

#### Scenario: Jump to stage
- **WHEN** the user presses Enter on a box
- **THEN** the pipeline view opens with that stage selected

#### Scenario: Larger than the screen
- **WHEN** the drawing is wider or taller than the available area
- **THEN** the view scrolls to keep the cursor visible

### Requirement: Graceful degradation
The system SHALL stay usable for projects without stages or with a very wide graph.

#### Scenario: No stages
- **WHEN** the project has no stages
- **THEN** the view says so instead of drawing an empty canvas

#### Scenario: Run keys
- **WHEN** the user presses `r` or `R` on a box
- **THEN** that stage runs exactly as it would from the pipeline view
