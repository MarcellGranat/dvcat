//! The stage dependency graph, derived from `dvc.yaml` outs and deps.
//!
//! Stages are referred to by their index in the `&[Stage]` slice, never by reference.

use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fmt,
    path::{Component, Path, PathBuf},
};

use crate::project::Stage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    /// `upstream[i]` holds the stages that stage `i` depends on, in ascending index order.
    pub upstream: Vec<Vec<usize>>,
    /// Stage indices with upstream stages first; ties keep `dvc.yaml` order.
    pub order: Vec<usize>,
}

/// The stages that can never be ordered because they sit in, or behind, a dependency cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleError {
    pub stages: Vec<String>,
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dependency cycle among stages: {}",
            self.stages.join(", ")
        )
    }
}

// An empty impl is enough: `Error` just needs `Debug + Display`. It lets `?` convert into anyhow.
impl std::error::Error for CycleError {}

pub fn build(stages: &[Stage]) -> Result<Graph, CycleError> {
    let upstream = find_upstream(stages);
    match topological_order(&upstream) {
        Ok(order) => Ok(Graph { upstream, order }),
        Err(stuck) => Err(CycleError {
            stages: stuck.into_iter().map(|i| stages[i].name.clone()).collect(),
        }),
    }
}

/// Drop `./` parts so `./data/x` and `data/x` compare equal.
fn normalize(path: &str) -> PathBuf {
    Path::new(path)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

fn find_upstream(stages: &[Stage]) -> Vec<Vec<usize>> {
    // Paths each stage produces. An empty path (from ".") would prefix-match everything, so skip it.
    let produced: Vec<Vec<PathBuf>> = stages
        .iter()
        .map(|s| {
            s.produced()
                .map(normalize)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .collect();

    stages
        .iter()
        .map(|stage| {
            let deps: Vec<PathBuf> = stage.deps.iter().map(|d| normalize(d)).collect();
            produced
                .iter()
                .enumerate()
                .filter(|(_, outs)| {
                    deps.iter()
                        .any(|dep| outs.iter().any(|out| dep.starts_with(out)))
                })
                .map(|(i, _)| i)
                .collect()
        })
        .collect()
}

/// Kahn's algorithm. `Err` carries the indices that could not be placed.
fn topological_order(upstream: &[Vec<usize>]) -> Result<Vec<usize>, Vec<usize>> {
    let n = upstream.len();
    // How many upstream stages each stage is still waiting for.
    let mut waiting_for: Vec<usize> = upstream.iter().map(Vec::len).collect();
    let mut downstream: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (stage, ups) in upstream.iter().enumerate() {
        for &up in ups {
            downstream[up].push(stage);
        }
    }

    let mut ready: BinaryHeap<Reverse<usize>> = (0..n)
        .filter(|&i| waiting_for[i] == 0)
        .map(Reverse)
        .collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(stage)) = ready.pop() {
        order.push(stage);
        for &next in &downstream[stage] {
            waiting_for[next] -= 1;
            if waiting_for[next] == 0 {
                ready.push(Reverse(next));
            }
        }
    }

    if order.len() == n {
        Ok(order)
    } else {
        Err((0..n).filter(|&i| waiting_for[i] > 0).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn stage(name: &str, deps: &[&str], outs: &[&str]) -> Stage {
        Stage {
            name: name.into(),
            cmd: String::new(),
            deps: deps.iter().map(|s| (*s).into()).collect(),
            params: Vec::new(),
            outs: outs.iter().map(|s| (*s).into()).collect(),
            metrics: Vec::new(),
            unsupported: None,
        }
    }

    fn names(stages: &[Stage], graph: &Graph) -> Vec<String> {
        graph
            .order
            .iter()
            .map(|&i| stages[i].name.clone())
            .collect()
    }

    #[test]
    fn upstream_comes_first_even_when_declared_last() {
        let stages = [
            stage("train", &["features.csv"], &["model"]),
            stage("features", &["prepared.csv"], &["features.csv"]),
            stage("prepare", &["raw.csv"], &["prepared.csv"]),
        ];
        let graph = build(&stages).unwrap();
        assert_eq!(names(&stages, &graph), ["prepare", "features", "train"]);
        assert_eq!(graph.upstream[0], [1]); // train <- features
        assert_eq!(graph.upstream[2], Vec::<usize>::new());
    }

    #[test]
    fn independent_stages_keep_declaration_order() {
        let stages = [stage("b", &[], &["b.txt"]), stage("a", &[], &["a.txt"])];
        let graph = build(&stages).unwrap();
        assert_eq!(names(&stages, &graph), ["b", "a"]);
    }

    #[test]
    fn dep_inside_an_output_directory_is_an_edge() {
        let stages = [
            stage("make", &[], &["data"]),
            stage("use", &["data/part1.csv"], &[]),
        ];
        assert_eq!(build(&stages).unwrap().upstream[1], [0]);
    }

    #[test]
    fn prefix_must_match_whole_path_components() {
        let stages = [
            stage("make", &[], &["data"]),
            stage("use", &["data2/x.csv"], &[]),
        ];
        assert!(build(&stages).unwrap().upstream[1].is_empty());
    }

    #[test]
    fn dot_slash_prefixes_are_ignored() {
        let stages = [
            stage("make", &[], &["./out/x"]),
            stage("use", &["out/x"], &[]),
        ];
        assert_eq!(build(&stages).unwrap().upstream[1], [0]);
    }

    #[test]
    fn metrics_count_as_produced_paths() {
        let mut maker = stage("eval", &[], &[]);
        maker.metrics = vec!["metrics.json".into()];
        let stages = [maker, stage("report", &["metrics.json"], &[])];
        assert_eq!(build(&stages).unwrap().upstream[1], [0]);
    }

    #[test]
    fn cycle_reports_the_stages_involved() {
        let stages = [
            stage("a", &["b.txt"], &["a.txt"]),
            stage("b", &["a.txt"], &["b.txt"]),
            stage("free", &[], &["f.txt"]),
        ];
        let err = build(&stages).unwrap_err();
        assert_eq!(err.stages, ["a", "b"]);
        assert!(err.to_string().contains("a, b"));
    }

    #[test]
    fn stage_depending_on_its_own_output_is_a_cycle() {
        let stages = [stage("loop", &["x"], &["x"])];
        assert_eq!(build(&stages).unwrap_err().stages, ["loop"]);
    }

    #[test]
    fn demo_fixture_orders_prepare_features_train() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo");
        let project = Project::load(&root).unwrap();
        let graph = build(&project.stages).unwrap();
        assert_eq!(
            names(&project.stages, &graph),
            ["prepare", "features", "train"]
        );
        assert_eq!(graph.upstream, [vec![], vec![0], vec![1]]);
    }

    #[test]
    fn expanded_stages_are_ordered_and_connected_like_hand_written_ones() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/matrix");
        let project = Project::load(&root).unwrap();
        let graph = build(&project.stages).unwrap();
        let order = names(&project.stages, &graph);
        let position = |name: &str| order.iter().position(|n| n == name).unwrap();
        assert_eq!(order.len(), 7, "{order:?}");
        assert!(position("each@us") < position("collect"));
        assert!(position("each@eu") < position("collect"));
        let collect = project
            .stages
            .iter()
            .position(|s| s.name == "collect")
            .unwrap();
        let upstream: Vec<&str> = graph.upstream[collect]
            .iter()
            .map(|&i| project.stages[i].name.as_str())
            .collect();
        assert_eq!(upstream, ["each@us", "each@eu"]);
    }
}
