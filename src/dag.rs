//! Layout of the stage graph for drawing: layers left to right, boxes stacked in each layer,
//! and lines routed between them on a character canvas.
//!
//! Pure and independent of the terminal: the UI reads the result and adds colours.
//!
//! Method (a small Sugiyama-style layout): a stage's layer is its longest distance from a stage
//! without upstream; an edge that skips layers is split by invisible "virtual" nodes so every
//! drawn edge crosses exactly one gap between adjacent layers; each layer is ordered by one
//! forward and one backward barycenter sweep; edges that change height get a vertical "track"
//! in the gap, shared only by edges with the same source or target.

use crate::graph::Graph;

/// Longest stage name shown; longer ones end in `…`.
pub const MAX_NAME: usize = 24;
/// Vertical distance between the tops of two boxes in a layer (box height 3 plus one blank row).
const ROW_PITCH: usize = 4;
const BOX_HEIGHT: usize = 3;
/// Border, space, glyph, space, (name), space, border.
const BOX_CHROME: usize = 6;
/// Cells between two layers besides the vertical tracks: a stub, and the arrow cell.
const GAP_BASE: usize = 3;

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// Where one stage's box is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub layer: usize,
}

impl Placed {
    pub const HEIGHT: u16 = BOX_HEIGHT as u16;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// One box per stage, indexed like the stage list.
    pub nodes: Vec<Placed>,
    pub width: u16,
    pub height: u16,
    /// Line characters (`─│┌┐└┘├┤┬┴┼▶`), `height` rows of `width` cells; boxes are not included.
    pub lines: Vec<Vec<char>>,
    /// Stage indices of each layer, top to bottom.
    pub layers: Vec<Vec<usize>>,
    /// Stages that depend on each stage (the inverse of `Graph::upstream`).
    pub downstream: Vec<Vec<usize>>,
    /// Copy of `Graph::upstream`, for navigation.
    pub upstream: Vec<Vec<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Upstream,
    Downstream,
    Up,
    Down,
}

/// Text shown inside a box for a stage name.
pub fn display_name(name: &str) -> String {
    if name.chars().count() <= MAX_NAME {
        name.to_owned()
    } else {
        let kept: String = name.chars().take(MAX_NAME - 1).collect();
        format!("{kept}…")
    }
}

pub fn layout(names: &[&str], graph: &Graph) -> Layout {
    let count = names.len();
    if count == 0 {
        return Layout::default();
    }

    // 1. Layers: longest path from a source, walking in dependency order.
    let mut layer = vec![0usize; count];
    for &stage in &graph.order {
        layer[stage] = graph.upstream[stage]
            .iter()
            .map(|&up| layer[up] + 1)
            .max()
            .unwrap_or(0);
    }
    let layer_count = layer.iter().max().copied().unwrap_or(0) + 1;

    // 2. Nodes 0..count are stages; later ids are virtual nodes on edges that skip layers.
    let mut node_layer = layer.clone();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for stage in 0..count {
        for &up in &graph.upstream[stage] {
            let mut previous = up;
            for between in layer[up] + 1..layer[stage] {
                let id = node_layer.len();
                node_layer.push(between);
                edges.push((previous, id));
                previous = id;
            }
            edges.push((previous, stage));
        }
    }
    let node_total = node_layer.len();

    // 3. Order inside layers.
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for (id, &l) in node_layer.iter().enumerate() {
        columns[l].push(id);
    }
    let mut position = vec![0usize; node_total];
    let refresh = |columns: &[Vec<usize>], position: &mut [usize]| {
        for column in columns {
            for (row, &id) in column.iter().enumerate() {
                position[id] = row;
            }
        }
    };
    refresh(&columns, &mut position);
    for l in 1..layer_count {
        sweep(&mut columns[l], &position, &edges, true);
        refresh(&columns, &mut position);
    }
    for l in (0..layer_count - 1).rev() {
        sweep(&mut columns[l], &position, &edges, false);
        refresh(&columns, &mut position);
    }

    // 4. Widths and heights.
    let label_width = |id: usize| display_name(names[id]).chars().count();
    let layer_width: Vec<usize> = columns
        .iter()
        .map(|column| {
            column
                .iter()
                .filter(|&&id| id < count)
                .map(|&id| label_width(id) + BOX_CHROME)
                .max()
                .unwrap_or(1)
        })
        .collect();
    let mid_row = |id: usize| position[id] * ROW_PITCH + 1;

    // 5. Tracks in each gap.
    struct Wire {
        from: usize,
        to: usize,
        low: usize,
        high: usize,
        track: Option<usize>,
    }
    let mut gaps: Vec<Vec<Wire>> = (0..layer_count.saturating_sub(1))
        .map(|_| Vec::new())
        .collect();
    for &(from, to) in &edges {
        let (y_from, y_to) = (mid_row(from), mid_row(to));
        gaps[node_layer[from]].push(Wire {
            from,
            to,
            low: y_from.min(y_to),
            high: y_from.max(y_to),
            track: None,
        });
    }
    let mut track_count = vec![0usize; gaps.len()];
    for (g, wires) in gaps.iter_mut().enumerate() {
        wires.sort_by_key(|w| (w.low, w.high, w.from, w.to));
        // Straight wires need no track; the others are packed greedily.
        let mut assigned: Vec<Vec<(usize, usize, usize, usize)>> = Vec::new();
        for wire in wires.iter_mut().filter(|w| w.low != w.high) {
            let fits = |track: &Vec<(usize, usize, usize, usize)>| {
                track.iter().all(|&(low, high, from, to)| {
                    let overlap = wire.low <= high && low <= wire.high;
                    !overlap || from == wire.from || to == wire.to
                })
            };
            let index = assigned.iter().position(fits).unwrap_or_else(|| {
                assigned.push(Vec::new());
                assigned.len() - 1
            });
            assigned[index].push((wire.low, wire.high, wire.from, wire.to));
            wire.track = Some(index);
        }
        track_count[g] = assigned.len();
    }

    // 6. Horizontal positions.
    let mut x_layer = vec![0usize; layer_count];
    for l in 1..layer_count {
        x_layer[l] = x_layer[l - 1] + layer_width[l - 1] + track_count[l - 1] + GAP_BASE;
    }
    let width = x_layer[layer_count - 1] + layer_width[layer_count - 1];
    // Tall enough for the lowest box or the lowest line of a virtual node.
    let height = (0..node_total)
        .map(|id| {
            if id < count {
                position[id] * ROW_PITCH + BOX_HEIGHT
            } else {
                mid_row(id) + 1
            }
        })
        .max()
        .unwrap_or(BOX_HEIGHT);

    // 7. Draw the lines.
    let mut mask = vec![vec![0u8; width]; height];
    let mut arrows: Vec<(usize, usize)> = Vec::new();
    let horizontal = |mask: &mut Vec<Vec<u8>>, x1: usize, x2: usize, y: usize| {
        let last = x2 - x1;
        for (offset, cell) in mask[y][x1..=x2].iter_mut().enumerate() {
            if offset > 0 {
                *cell |= LEFT;
            }
            if offset < last {
                *cell |= RIGHT;
            }
        }
    };
    let vertical = |mask: &mut Vec<Vec<u8>>, x: usize, y1: usize, y2: usize| {
        let (low, high) = (y1.min(y2), y1.max(y2));
        let last = high - low;
        for (offset, row) in mask[low..=high].iter_mut().enumerate() {
            if offset > 0 {
                row[x] |= UP;
            }
            if offset < last {
                row[x] |= DOWN;
            }
        }
    };
    // A virtual node is a stretch of line across its (narrow) layer.
    for (id, &l) in node_layer.iter().enumerate().skip(count) {
        horizontal(
            &mut mask,
            x_layer[l],
            x_layer[l] + layer_width[l] - 1,
            mid_row(id),
        );
    }
    for (g, wires) in gaps.iter().enumerate() {
        let x_start = x_layer[g] + layer_width[g];
        let x_arrow = x_layer[g + 1] - 1;
        for wire in wires {
            let (y_from, y_to) = (mid_row(wire.from), mid_row(wire.to));
            match wire.track {
                None => horizontal(&mut mask, x_start, x_arrow, y_from),
                Some(track) => {
                    let x_track = x_start + 1 + track;
                    horizontal(&mut mask, x_start, x_track, y_from);
                    vertical(&mut mask, x_track, y_from, y_to);
                    horizontal(&mut mask, x_track, x_arrow, y_to);
                }
            }
            if wire.to < count {
                arrows.push((x_arrow, y_to));
            }
        }
    }
    let mut lines: Vec<Vec<char>> = mask
        .iter()
        .map(|row| row.iter().map(|&bits| line_char(bits)).collect())
        .collect();
    for (x, y) in arrows {
        lines[y][x] = '▶';
    }

    // 8. Results for stages only.
    let nodes = (0..count)
        .map(|stage| Placed {
            x: x_layer[layer[stage]] as u16,
            y: (position[stage] * ROW_PITCH) as u16,
            w: layer_width[layer[stage]] as u16,
            layer: layer[stage],
        })
        .collect();
    let layers = columns
        .iter()
        .map(|column| column.iter().copied().filter(|&id| id < count).collect())
        .collect();
    let mut downstream = vec![Vec::new(); count];
    for (stage, ups) in graph.upstream.iter().enumerate() {
        for &up in ups {
            downstream[up].push(stage);
        }
    }
    Layout {
        nodes,
        width: width as u16,
        height: height as u16,
        lines,
        layers,
        downstream,
        upstream: graph.upstream.clone(),
    }
}

/// Reorder `column` by the average row of its neighbours in the adjacent layer
/// (predecessors when `forward`, successors otherwise); nodes without neighbours keep their row.
fn sweep(column: &mut [usize], position: &[usize], edges: &[(usize, usize)], forward: bool) {
    let key = |id: usize| -> f64 {
        let neighbours: Vec<usize> = edges
            .iter()
            .filter_map(|&(from, to)| match forward {
                true if to == id => Some(position[from]),
                false if from == id => Some(position[to]),
                _ => None,
            })
            .collect();
        if neighbours.is_empty() {
            position[id] as f64
        } else {
            neighbours.iter().sum::<usize>() as f64 / neighbours.len() as f64
        }
    };
    // Stable: equal keys keep their current order.
    let mut keyed: Vec<(f64, usize)> = column.iter().map(|&id| (key(id), id)).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (slot, (_, id)) in column.iter_mut().zip(keyed) {
        *slot = id;
    }
}

fn line_char(bits: u8) -> char {
    match bits {
        0 => ' ',
        b if b == UP | DOWN || b == UP || b == DOWN => '│',
        b if b == LEFT | RIGHT || b == LEFT || b == RIGHT => '─',
        b if b == DOWN | RIGHT => '┌',
        b if b == DOWN | LEFT => '┐',
        b if b == UP | RIGHT => '└',
        b if b == UP | LEFT => '┘',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == LEFT | RIGHT | DOWN => '┬',
        b if b == LEFT | RIGHT | UP => '┴',
        _ => '┼',
    }
}

impl Layout {
    /// Move from `stage` to a neighbour: along an edge, or to the box above or below in its layer.
    /// Stays put when there is no neighbour that way.
    pub fn step(&self, stage: usize, direction: Move) -> usize {
        let Some(here) = self.nodes.get(stage) else {
            return stage;
        };
        let closest = |candidates: &[usize]| {
            candidates
                .iter()
                .copied()
                .min_by_key(|&c| (self.nodes[c].y.abs_diff(here.y), self.nodes[c].y))
                .unwrap_or(stage)
        };
        match direction {
            Move::Upstream => closest(&self.upstream[stage]),
            Move::Downstream => closest(&self.downstream[stage]),
            Move::Up | Move::Down => {
                let column = &self.layers[here.layer];
                let index = column.iter().position(|&s| s == stage).unwrap_or(0);
                let target = if direction == Move::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(column.len() - 1)
                };
                column[target]
            }
        }
    }

    /// The whole drawing as plain text rows (boxes with `glyph` for the status), for tests.
    pub fn to_text(&self, names: &[&str], glyph: impl Fn(usize) -> char) -> Vec<String> {
        let mut grid = self.lines.clone();
        for (stage, node) in self.nodes.iter().enumerate() {
            let (x, y, w) = (
                usize::from(node.x),
                usize::from(node.y),
                usize::from(node.w),
            );
            let name = display_name(names[stage]);
            let inner = w - 2;
            let text = format!(" {} {:<width$} ", glyph(stage), name, width = inner - 4);
            let rows = [
                format!("╭{}╮", "─".repeat(inner)),
                format!("│{text}│"),
                format!("╰{}╯", "─".repeat(inner)),
            ];
            for (dy, row) in rows.iter().enumerate() {
                for (dx, c) in row.chars().enumerate() {
                    grid[y + dy][x + dx] = c;
                }
            }
        }
        grid.into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph from `(stage, upstream stages)` pairs given in dependency order.
    fn graph(upstream: &[&[usize]]) -> Graph {
        let upstream: Vec<Vec<usize>> = upstream.iter().map(|u| u.to_vec()).collect();
        // The tests list stages in dependency order, so declaration order is a valid order.
        let order = (0..upstream.len()).collect();
        Graph { upstream, order }
    }

    fn draw(names: &[&str], upstream: &[&[usize]]) -> Vec<String> {
        layout(names, &graph(upstream)).to_text(names, |_| '●')
    }

    fn show(rows: &[String]) -> String {
        rows.join("\n")
    }

    #[test]
    fn an_empty_project_has_an_empty_layout() {
        let layout = layout(&[], &graph(&[]));
        assert_eq!((layout.width, layout.height), (0, 0));
        assert!(layout.nodes.is_empty());
    }

    #[test]
    fn a_single_stage_is_one_box() {
        let rows = draw(&["only"], &[&[]]);
        assert_eq!(show(&rows), "╭────────╮\n│ ● only │\n╰────────╯");
    }

    #[test]
    fn a_chain_reads_left_to_right_with_arrows() {
        let rows = draw(&["prepare", "train"], &[&[], &[0]]);
        assert_eq!(
            show(&rows),
            "╭───────────╮   ╭─────────╮\n\
             │ ● prepare │──▶│ ● train │\n\
             ╰───────────╯   ╰─────────╯"
        );
    }

    #[test]
    fn a_fork_shares_one_track_leaving_the_source() {
        let rows = draw(&["a", "b", "c"], &[&[], &[0], &[0]]);
        assert_eq!(
            show(&rows),
            "╭─────╮    ╭─────╮\n\
             │ ● a │─┬─▶│ ● b │\n\
             ╰─────╯ │  ╰─────╯\n\
             \x20       │\n\
             \x20       │  ╭─────╮\n\
             \x20       └─▶│ ● c │\n\
             \x20          ╰─────╯"
        );
    }

    #[test]
    fn a_join_merges_into_one_arrow() {
        let rows = draw(&["a", "b", "c"], &[&[], &[], &[0, 1]]);
        assert_eq!(
            show(&rows),
            "╭─────╮    ╭─────╮\n\
             │ ● a │─┬─▶│ ● c │\n\
             ╰─────╯ │  ╰─────╯\n\
             \x20       │\n\
             ╭─────╮ │\n\
             │ ● b │─┘\n\
             ╰─────╯"
        );
    }

    #[test]
    fn a_skipped_layer_is_routed_below_the_box_in_between() {
        // a -> b -> c and a -> c: the long edge goes around b through a virtual row, then
        // joins b's edge in front of c's arrowhead.
        let rows = draw(&["a", "b", "c"], &[&[], &[0], &[0, 1]]);
        assert_eq!(
            show(&rows),
            "╭─────╮    ╭─────╮    ╭─────╮\n\
             │ ● a │─┬─▶│ ● b │─┬─▶│ ● c │\n\
             ╰─────╯ │  ╰─────╯ │  ╰─────╯\n\
             \x20       │          │\n\
             \x20       │          │\n\
             \x20       └──────────┘"
        );
    }

    #[test]
    fn a_diamond_has_both_branches_and_one_arrow_into_the_last_stage() {
        let rows = draw(&["a", "b", "c", "d"], &[&[], &[0], &[0], &[1, 2]]);
        let text = show(&rows);
        for name in ["a", "b", "c", "d"] {
            assert!(text.contains(&format!("● {name} │")), "{text}");
        }
        // Edges: a->b, a->c, b->d, c->d. Arrowheads: into b, into c, and one shared into d.
        assert_eq!(text.matches('▶').count(), 3, "{text}");
    }

    #[test]
    fn boxes_never_overlap_and_lines_never_enter_boxes() {
        // A busier graph: two sources, a join, a skip, and a final fan-in.
        let names = ["raw", "ref", "clean", "feat", "train", "report"];
        let upstream: [&[usize]; 6] = [&[], &[], &[0], &[2, 1], &[3, 0], &[4, 3]];
        let g = graph(&upstream);
        let layout = layout(&names, &g);
        for (i, a) in layout.nodes.iter().enumerate() {
            for b in layout.nodes.iter().skip(i + 1) {
                let apart_x = a.x + a.w <= b.x || b.x + b.w <= a.x;
                let apart_y = a.y + Placed::HEIGHT <= b.y || b.y + Placed::HEIGHT <= a.y;
                assert!(apart_x || apart_y, "{a:?} overlaps {b:?}");
            }
            for dy in 0..Placed::HEIGHT {
                for dx in 0..a.w {
                    let c = layout.lines[usize::from(a.y + dy)][usize::from(a.x + dx)];
                    assert_eq!(c, ' ', "a line cell inside the box of stage {i}");
                }
            }
        }
        // Edges into one stage meet in front of a single arrowhead.
        let arrows = layout.lines.iter().flatten().filter(|&&c| c == '▶').count();
        let with_upstream = g.upstream.iter().filter(|u| !u.is_empty()).count();
        assert_eq!(arrows, with_upstream);
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(display_name("short"), "short");
        let long = "x".repeat(40);
        let shown = display_name(&long);
        assert_eq!(shown.chars().count(), MAX_NAME);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn navigation_follows_edges_and_layers() {
        let names = ["a", "b", "c"];
        let g = graph(&[&[], &[0], &[0]]);
        let layout = layout(&names, &g);
        // a -> {b, c}; b above c in layer 1.
        assert_eq!(layout.step(0, Move::Downstream), 1, "closest downstream");
        assert_eq!(layout.step(1, Move::Upstream), 0);
        assert_eq!(layout.step(1, Move::Down), 2);
        assert_eq!(layout.step(2, Move::Up), 1);
        assert_eq!(layout.step(1, Move::Up), 1, "stays at the top");
        assert_eq!(layout.step(0, Move::Upstream), 0, "no upstream: stays");
        assert_eq!(layout.step(2, Move::Down), 2);
    }

    #[test]
    fn real_projects_lay_out_without_panicking() {
        for fixture in ["demo", "matrix", "articleiv"] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture);
            let project = crate::project::Project::load(&root).unwrap();
            let g = crate::graph::build(&project.stages).unwrap();
            let names: Vec<&str> = project.stages.iter().map(|s| s.name.as_str()).collect();
            let layout = layout(&names, &g);
            assert_eq!(layout.nodes.len(), names.len());
            assert_eq!(layout.lines.len(), usize::from(layout.height));
        }
    }
}
