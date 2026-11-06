//! Is each stage up to date? Compares `dvc.lock` (state at the last run) with the workspace,
//! without running anything, and keeps the *reasons* so the UI can explain itself.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

use md5::{Digest, Md5};
use serde_json::Value;

use crate::{
    graph::Graph,
    project::{LockStage, Project, Stage, flatten_leaves, param_value},
};

/// One concrete finding. Each variant carries exactly the facts needed to explain it.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    NeverRun,
    CmdChanged {
        old: String,
        new: String,
    },
    /// `None` means "absent": missing from the lock (newly tracked) or from the param file (removed).
    ParamChanged {
        file: String,
        key: String,
        locked: Option<Value>,
        current: Option<Value>,
    },
    DepChanged {
        path: String,
    },
    DepMissing {
        path: String,
    },
    DepAdded {
        path: String,
    },
    DepRemoved {
        path: String,
    },
    /// A directory dependency: its hash needs DVC's directory manifest, which we do not compute.
    DirUnverified {
        path: String,
    },
    /// A recorded output no longer exists in the workspace.
    OutMissing {
        path: String,
    },
    /// A recorded output exists but its size or content differs from the lock.
    OutModified {
        path: String,
    },
    /// An output we could not check: a directory, or a file too large to hash.
    OutUnverified {
        path: String,
    },
    Upstream {
        stage: String,
    },
    UpstreamUnverified {
        stage: String,
    },
}

impl Reason {
    /// Reasons that mean "we could not check", as opposed to "something changed".
    fn is_unverified(&self) -> bool {
        matches!(
            self,
            Reason::DirUnverified { .. }
                | Reason::OutUnverified { .. }
                | Reason::UpstreamUnverified { .. }
        )
    }
}

/// Shown for a value that is not there.
const ABSENT: &str = "(absent)";
const REMOVED: &str = "(removed)";

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `match` must cover every variant: adding a `Reason` without a message will not compile.
        match self {
            Reason::NeverRun => write!(f, "never run"),
            Reason::CmdChanged { old, new } => write!(f, "cmd: {old} → {new}"),
            Reason::ParamChanged {
                key,
                locked,
                current,
                ..
            } => {
                let old = locked.as_ref().map_or(ABSENT.to_owned(), Value::to_string);
                let new = current
                    .as_ref()
                    .map_or(REMOVED.to_owned(), Value::to_string);
                write!(f, "{key}: {old} → {new}")
            }
            Reason::DepChanged { path } => write!(f, "changed: {path}"),
            Reason::DepMissing { path } => write!(f, "missing: {path}"),
            Reason::DepAdded { path } => write!(f, "new dependency: {path}"),
            Reason::DepRemoved { path } => write!(f, "dependency removed: {path}"),
            Reason::DirUnverified { path } => write!(f, "unverified directory: {path}"),
            Reason::OutMissing { path } => write!(f, "missing output: {path}"),
            Reason::OutModified { path } => write!(f, "modified output: {path}"),
            Reason::OutUnverified { path } => write!(f, "unverified output: {path}"),
            Reason::Upstream { stage } => write!(f, "upstream: {stage}"),
            Reason::UpstreamUnverified { stage } => write!(f, "upstream unverified: {stage}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    UpToDate,
    Stale,
    NeverRun,
    Unverified,
}

/// The findings for one stage. The [`Status`] is derived from them, never stored separately.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageStatus {
    pub reasons: Vec<Reason>,
}

impl StageStatus {
    pub fn status(&self) -> Status {
        if self.reasons.contains(&Reason::NeverRun) {
            Status::NeverRun
        } else if self.reasons.iter().any(|r| !r.is_unverified()) {
            Status::Stale
        } else if self.reasons.is_empty() {
            Status::UpToDate
        } else {
            Status::Unverified
        }
    }
}

type ParamFiles = BTreeMap<String, Result<Value, String>>;

/// Statuses for every stage, indexed like `project.stages`.
/// Upstream propagation walks the stages in dependency order, so each stage sees final upstream results.
pub fn compute(project: &Project, graph: &Graph) -> Vec<StageStatus> {
    let mut statuses = vec![StageStatus::default(); project.stages.len()];
    for &index in &graph.order {
        let stage = &project.stages[index];
        let locked = project.lock.stages.get(&stage.name);
        let mut reasons = own_reasons(stage, locked, &project.root, &project.params);

        for &up in &graph.upstream[index] {
            let stage_name = project.stages[up].name.clone();
            match statuses[up].status() {
                Status::Stale | Status::NeverRun => {
                    reasons.push(Reason::Upstream { stage: stage_name })
                }
                Status::Unverified => {
                    reasons.push(Reason::UpstreamUnverified { stage: stage_name });
                }
                Status::UpToDate => {}
            }
        }
        statuses[index] = StageStatus { reasons };
    }
    statuses
}

/// What is wrong with this stage on its own, ignoring other stages.
pub fn own_reasons(
    stage: &Stage,
    locked: Option<&LockStage>,
    root: &Path,
    params: &ParamFiles,
) -> Vec<Reason> {
    let Some(locked) = locked else {
        return vec![Reason::NeverRun];
    };
    let mut reasons = Vec::new();

    let locked_cmd = locked.cmd.as_ref().map(|c| c.joined()).unwrap_or_default();
    if locked_cmd != stage.cmd {
        reasons.push(Reason::CmdChanged {
            old: locked_cmd,
            new: stage.cmd.clone(),
        });
    }
    reasons.extend(dep_reasons(stage, locked, root));
    reasons.extend(param_reasons(stage, locked, params));
    reasons.extend(out_reasons(stage, locked, root, HASH_LIMIT));
    reasons
}

// ---------------------------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------------------------

/// `./a/b/` and `a/b` are the same path.
fn clean(path: &str) -> &str {
    path.trim_start_matches("./").trim_end_matches('/')
}

fn dep_reasons(stage: &Stage, locked: &LockStage, root: &Path) -> Vec<Reason> {
    let mut reasons = Vec::new();

    for dep in &stage.deps {
        let Some(entry) = locked.deps.iter().find(|d| clean(&d.path) == clean(dep)) else {
            reasons.push(Reason::DepAdded { path: dep.clone() });
            continue;
        };
        let on_disk = root.join(clean(dep));
        let locked_md5 = entry.md5.as_deref();

        if !on_disk.exists() {
            reasons.push(Reason::DepMissing { path: dep.clone() });
        } else if on_disk.is_dir() || locked_md5.is_some_and(|m| m.ends_with(".dir")) {
            reasons.push(Reason::DirUnverified { path: dep.clone() });
        } else {
            match md5_of_file(&on_disk) {
                Ok(now) if Some(now.as_str()) == locked_md5 => {}
                Ok(_) => reasons.push(Reason::DepChanged { path: dep.clone() }),
                Err(_) => reasons.push(Reason::DepMissing { path: dep.clone() }),
            }
        }
    }

    for entry in &locked.deps {
        if !stage.deps.iter().any(|d| clean(d) == clean(&entry.path)) {
            reasons.push(Reason::DepRemoved {
                path: entry.path.clone(),
            });
        }
    }
    reasons
}

// ---------------------------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------------------------

/// Files larger than this are not hashed to check an output; they are reported as unverified
/// when their size matches the lock. (A size mismatch is still reported as modified.)
pub const HASH_LIMIT: u64 = 256 * 1024 * 1024;

/// Compare the outputs recorded in the lock (and still declared by the stage) with the workspace.
/// The size recorded in the lock decides most cases without reading the file.
fn out_reasons(stage: &Stage, locked: &LockStage, root: &Path, hash_limit: u64) -> Vec<Reason> {
    let mut reasons = Vec::new();
    for entry in &locked.outs {
        // An output that is no longer declared is not this stage's concern any more.
        if !stage.produced().any(|p| clean(p) == clean(&entry.path)) {
            continue;
        }
        let Some(locked_md5) = entry.md5.as_deref() else {
            continue; // recorded without a hash: nothing to compare with
        };
        let path = entry.path.clone();
        let on_disk = root.join(clean(&entry.path));
        let Ok(metadata) = fs::metadata(&on_disk) else {
            reasons.push(Reason::OutMissing { path });
            continue;
        };
        if metadata.is_dir() || locked_md5.ends_with(".dir") {
            reasons.push(Reason::OutUnverified { path });
        } else if entry.size.is_some_and(|size| size != metadata.len()) {
            reasons.push(Reason::OutModified { path });
        } else if metadata.len() > hash_limit {
            reasons.push(Reason::OutUnverified { path });
        } else {
            match md5_of_file(&on_disk) {
                Ok(now) if now == locked_md5 => {}
                Ok(_) => reasons.push(Reason::OutModified { path }),
                Err(_) => reasons.push(Reason::OutMissing { path }),
            }
        }
    }
    reasons
}

fn md5_of_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------------------------

fn param_reasons(stage: &Stage, locked: &LockStage, params: &ParamFiles) -> Vec<Reason> {
    // Files the stage tracks now, plus files it tracked at the last run (a file may have been dropped).
    let files: BTreeSet<&str> = stage
        .params
        .iter()
        .map(|p| p.file.as_str())
        .chain(locked.params.keys().map(String::as_str))
        .collect();

    let mut reasons = Vec::new();
    for file in files {
        let old = locked_leaves(locked, file);
        let new = current_leaves(stage, params, file);
        let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
        for key in keys {
            let (old_value, new_value) = (old.get(key).copied(), new.get(key).copied());
            if !same_value(old_value, new_value) {
                reasons.push(Reason::ParamChanged {
                    file: file.to_owned(),
                    key: key.clone(),
                    locked: old_value.cloned(),
                    current: new_value.cloned(),
                });
            }
        }
    }
    reasons
}

/// Leaves recorded in the lock for one file. Flat dotted keys and nested sections both end up flat.
fn locked_leaves<'a>(locked: &'a LockStage, file: &str) -> BTreeMap<String, &'a Value> {
    locked
        .params
        .get(file)
        .into_iter()
        .flatten()
        .flat_map(|(key, value)| flatten_leaves(key, value))
        .collect()
}

/// Leaves of the keys the stage tracks, read from the current param file.
fn current_leaves<'a>(
    stage: &Stage,
    params: &'a ParamFiles,
    file: &str,
) -> BTreeMap<String, &'a Value> {
    let Some(Ok(tree)) = params.get(file) else {
        return BTreeMap::new();
    };
    let mut leaves = BTreeMap::new();
    for param in stage.params.iter().filter(|p| p.file == file) {
        if param.keys.is_empty() {
            // Whole file: every top-level entry counts.
            if let Value::Object(map) = tree {
                for (key, value) in map {
                    leaves.extend(flatten_leaves(key, value));
                }
            }
        } else {
            for key in &param.keys {
                if let Some(value) = param_value(tree, key) {
                    leaves.extend(flatten_leaves(key, value));
                }
            }
        }
    }
    leaves
}

/// Numbers compare by value: YAML may say `5` where DVC wrote `5.0`, and they are the same param.
fn same_value(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{
        graph,
        test_support::{copy_dir, fixture},
    };

    /// A scratch copy of the (committed, consistent) demo project to tamper with.
    fn demo_copy() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture("demo"), dir.path());
        let root = dir.path().to_path_buf();
        (dir, root)
    }

    fn statuses(root: &Path) -> (Project, Vec<StageStatus>) {
        let project = Project::load(root).unwrap();
        let graph = graph::build(&project.stages).unwrap();
        let statuses = compute(&project, &graph);
        (project, statuses)
    }

    fn of<'a>(project: &Project, statuses: &'a [StageStatus], name: &str) -> &'a StageStatus {
        let index = project.stages.iter().position(|s| s.name == name).unwrap();
        &statuses[index]
    }

    fn texts(status: &StageStatus) -> Vec<String> {
        status.reasons.iter().map(ToString::to_string).collect()
    }

    fn replace_in(path: &Path, from: &str, to: &str) {
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains(from), "{from:?} not in {}", path.display());
        fs::write(path, text.replace(from, to)).unwrap();
    }

    // ---- 5.1: never run, cmd, params ----

    #[test]
    fn untouched_demo_is_up_to_date() {
        let (_dir, root) = demo_copy();
        let (_, statuses) = statuses(&root);
        assert!(
            statuses.iter().all(|s| s.status() == Status::UpToDate),
            "{statuses:#?}"
        );
    }

    #[test]
    fn missing_lock_file_means_every_stage_never_ran() {
        let (_dir, root) = demo_copy();
        fs::remove_file(root.join("dvc.lock")).unwrap();
        let (_, statuses) = statuses(&root);
        assert!(statuses.iter().all(|s| s.status() == Status::NeverRun));
    }

    #[test]
    fn stage_absent_from_lock_never_ran() {
        let (_dir, root) = demo_copy();
        replace_in(&root.join("dvc.lock"), "  train:", "  renamed:");
        let (project, statuses) = statuses(&root);
        assert_eq!(of(&project, &statuses, "train").status(), Status::NeverRun);
        assert_eq!(
            of(&project, &statuses, "prepare").status(),
            Status::UpToDate
        );
    }

    #[test]
    fn changed_param_is_reported_with_old_and_new_value() {
        let (_dir, root) = demo_copy();
        replace_in(&root.join("params.yaml"), "lr: 0.01", "lr: 0.05");
        let (project, statuses) = statuses(&root);
        let train = of(&project, &statuses, "train");
        assert_eq!(train.status(), Status::Stale);
        assert_eq!(texts(train), ["train.lr: 0.01 → 0.05"]);
        // Stages that do not track this param are unaffected.
        assert_eq!(
            of(&project, &statuses, "features").status(),
            Status::UpToDate
        );
    }

    #[test]
    fn int_and_float_spellings_of_the_same_number_are_equal() {
        let (_dir, root) = demo_copy();
        replace_in(&root.join("params.yaml"), "epochs: 5", "epochs: 5.0");
        let (_, statuses) = statuses(&root);
        assert!(
            statuses.iter().all(|s| s.status() == Status::UpToDate),
            "{statuses:#?}"
        );
    }

    #[test]
    fn removed_param_key_is_stale_and_shown_as_removed() {
        let (_dir, root) = demo_copy();
        replace_in(&root.join("params.yaml"), "  epochs: 5\n", "");
        let (project, statuses) = statuses(&root);
        let train = of(&project, &statuses, "train");
        assert_eq!(train.status(), Status::Stale);
        assert_eq!(texts(train), ["train.epochs: 5 → (removed)"]);
    }

    #[test]
    fn newly_tracked_param_is_stale() {
        let (_dir, root) = demo_copy();
        replace_in(
            &root.join("dvc.yaml"),
            "      - features.scale\n",
            "      - features.scale\n      - train.lr\n",
        );
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "features")),
            ["train.lr: (absent) → 0.01"]
        );
    }

    #[test]
    fn changed_cmd_shows_old_and_new_command() {
        let (_dir, root) = demo_copy();
        replace_in(
            &root.join("dvc.yaml"),
            "python3 src/train.py",
            "python3 src/train.py --fast",
        );
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "train")),
            ["cmd: python3 src/train.py → python3 src/train.py --fast"]
        );
    }

    #[test]
    fn articleiv_download_limit_3_to_null_is_the_headline_case() {
        let (project, statuses) = statuses(&fixture("articleiv"));
        let download = of(&project, &statuses, "download");
        assert_eq!(download.status(), Status::Stale);
        assert!(
            texts(download).contains(&"download.limit: 3 → null".to_owned()),
            "{:?}",
            texts(download)
        );
    }

    // ---- 5.2: dependency hashes ----

    #[test]
    fn edited_dependency_is_changed() {
        let (_dir, root) = demo_copy();
        fs::write(root.join("src/prepare.py"), "# edited\n").unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "prepare")),
            ["changed: src/prepare.py"]
        );
    }

    #[test]
    fn missing_dependency_is_reported_missing() {
        let (_dir, root) = demo_copy();
        fs::remove_file(root.join("data/raw.csv")).unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "prepare")),
            ["missing: data/raw.csv"]
        );
    }

    #[test]
    fn added_and_removed_dependencies_are_stale() {
        let (_dir, root) = demo_copy();
        replace_in(
            &root.join("dvc.yaml"),
            "      - src/prepare.py\n",
            "      - src/prepare.py\n      - src/params.py\n",
        );
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "prepare")),
            ["new dependency: src/params.py"]
        );

        let (_dir2, root2) = demo_copy();
        replace_in(&root2.join("dvc.yaml"), "      - data/raw.csv\n", "");
        let (project, statuses) = self::statuses(&root2);
        assert_eq!(
            texts(of(&project, &statuses, "prepare")),
            ["dependency removed: data/raw.csv"]
        );
    }

    #[test]
    fn directory_dependency_is_unverified_not_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("inputs")).unwrap();
        fs::write(
            root.join("dvc.yaml"),
            "stages:\n  a:\n    cmd: x\n    deps: [inputs]\n",
        )
        .unwrap();
        fs::write(
            root.join("dvc.lock"),
            "schema: '2.0'\nstages:\n  a:\n    cmd: x\n    deps:\n    - path: inputs\n      md5: abc123.dir\n",
        )
        .unwrap();
        let (project, statuses) = statuses(root);
        let a = of(&project, &statuses, "a");
        assert_eq!(a.status(), Status::Unverified);
        assert_eq!(texts(a), ["unverified directory: inputs"]);
    }

    // ---- 5.3: upstream propagation ----

    #[test]
    fn staleness_propagates_downstream_with_the_upstream_name() {
        let (_dir, root) = demo_copy();
        fs::write(root.join("src/prepare.py"), "# edited\n").unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(of(&project, &statuses, "prepare").status(), Status::Stale);
        assert_eq!(
            texts(of(&project, &statuses, "features")),
            ["upstream: prepare"]
        );
        assert_eq!(
            texts(of(&project, &statuses, "train")),
            ["upstream: features"]
        );
    }

    #[test]
    fn upstream_never_run_makes_downstream_stale() {
        let (_dir, root) = demo_copy();
        replace_in(&root.join("dvc.lock"), "  prepare:", "  other:");
        let (project, statuses) = statuses(&root);
        assert_eq!(
            of(&project, &statuses, "prepare").status(),
            Status::NeverRun
        );
        assert_eq!(of(&project, &statuses, "features").status(), Status::Stale);
    }

    #[test]
    fn unverified_upstream_makes_downstream_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("inputs")).unwrap();
        fs::write(
            root.join("dvc.yaml"),
            "stages:\n  a:\n    cmd: x\n    deps: [inputs]\n    outs: [a.txt]\n  b:\n    cmd: y\n    deps: [a.txt]\n",
        )
        .unwrap();
        fs::write(root.join("a.txt"), "hi").unwrap();
        let hash = md5_of_file(&root.join("a.txt")).unwrap();
        fs::write(
            root.join("dvc.lock"),
            format!(
                "schema: '2.0'\nstages:\n  a:\n    cmd: x\n    deps:\n    - path: inputs\n      md5: abc.dir\n  b:\n    cmd: y\n    deps:\n    - path: a.txt\n      md5: {hash}\n"
            ),
        )
        .unwrap();
        let (project, statuses) = statuses(root);
        assert_eq!(of(&project, &statuses, "b").status(), Status::Unverified);
        assert_eq!(
            texts(of(&project, &statuses, "b")),
            ["upstream unverified: a"]
        );
    }

    #[test]
    fn md5_matches_the_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, "hello\n").unwrap();
        assert_eq!(
            md5_of_file(&file).unwrap(),
            "b1946ac92492d2347c6235b4d2611184"
        );
    }

    #[test]
    fn reasons_have_readable_messages() {
        let cases = [
            (Reason::NeverRun, "never run"),
            (Reason::Upstream { stage: "x".into() }, "upstream: x"),
            (Reason::DepMissing { path: "p".into() }, "missing: p"),
        ];
        for (reason, expected) in cases {
            assert_eq!(reason.to_string(), expected);
        }
    }

    // ---- outputs ----

    #[test]
    fn a_deleted_output_makes_the_stage_stale() {
        let (_dir, root) = demo_copy();
        fs::remove_file(root.join("model.txt")).unwrap();
        let (project, statuses) = statuses(&root);
        let train = of(&project, &statuses, "train");
        assert_eq!(train.status(), Status::Stale);
        assert_eq!(texts(train), ["missing output: model.txt"]);
    }

    #[test]
    fn an_edited_output_with_a_different_size_is_modified_without_hashing() {
        let (_dir, root) = demo_copy();
        fs::write(
            root.join("model.txt"),
            "much longer than before, certainly\n",
        )
        .unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "train")),
            ["modified output: model.txt"]
        );
    }

    #[test]
    fn an_edit_that_keeps_the_size_is_caught_by_the_hash() {
        let (_dir, root) = demo_copy();
        let original = fs::read(root.join("data/prepared.csv")).unwrap();
        let mut edited = original.clone();
        edited[0] = if edited[0] == b'x' { b'y' } else { b'x' };
        fs::write(root.join("data/prepared.csv"), edited).unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(
            texts(of(&project, &statuses, "prepare")),
            ["modified output: data/prepared.csv"]
        );
    }

    #[test]
    fn a_stale_output_makes_downstream_stages_stale() {
        let (_dir, root) = demo_copy();
        fs::remove_file(root.join("data/prepared.csv")).unwrap();
        let (project, statuses) = statuses(&root);
        assert_eq!(of(&project, &statuses, "prepare").status(), Status::Stale);
        assert_eq!(of(&project, &statuses, "features").status(), Status::Stale);
        assert!(
            texts(of(&project, &statuses, "features")).contains(&"upstream: prepare".to_owned())
        );
    }

    #[test]
    fn untouched_outputs_add_no_reason() {
        let (_dir, root) = demo_copy();
        let project = Project::load(&root).unwrap();
        for stage in &project.stages {
            let locked = project.lock.stages.get(&stage.name).unwrap();
            assert!(
                out_reasons(stage, locked, &root, HASH_LIMIT).is_empty(),
                "{}",
                stage.name
            );
        }
    }

    #[test]
    fn files_over_the_limit_are_unverified_not_hashed() {
        let (_dir, root) = demo_copy();
        let project = Project::load(&root).unwrap();
        let train = project.stages.iter().find(|s| s.name == "train").unwrap();
        let locked = project.lock.stages.get("train").unwrap();
        let reasons = out_reasons(train, locked, &root, 1); // every non-empty file is "too large"
        assert!(
            reasons
                .iter()
                .all(|r| matches!(r, Reason::OutUnverified { .. }))
                && !reasons.is_empty(),
            "{reasons:?}"
        );
        assert_eq!(StageStatus { reasons }.status(), Status::Unverified);
    }

    #[test]
    fn directory_outputs_are_unverified_never_modified_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("out")).unwrap();
        fs::write(root.join("out/a.txt"), "x").unwrap();
        fs::write(
            root.join("dvc.yaml"),
            "stages:\n  s:\n    cmd: x\n    outs:\n      - out\n",
        )
        .unwrap();
        fs::write(
            root.join("dvc.lock"),
            "schema: '2.0'\nstages:\n  s:\n    cmd: x\n    outs:\n    - path: out\n      hash: md5\n      md5: abc.dir\n      size: 1\n      nfiles: 1\n",
        )
        .unwrap();
        let (project, statuses) = statuses(root);
        assert_eq!(
            texts(of(&project, &statuses, "s")),
            ["unverified output: out"]
        );
        assert_eq!(of(&project, &statuses, "s").status(), Status::Unverified);
    }

    #[test]
    fn outputs_without_a_recorded_hash_and_undeclared_outputs_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("dvc.yaml"),
            "stages:\n  s:\n    cmd: x\n    outs:\n      - kept.txt\n",
        )
        .unwrap();
        fs::write(
            root.join("dvc.lock"),
            "schema: '2.0'\nstages:\n  s:\n    cmd: x\n    outs:\n    - path: kept.txt\n    - path: dropped.txt\n      md5: 00\n      size: 1\n",
        )
        .unwrap();
        let (project, statuses) = statuses(root);
        assert_eq!(of(&project, &statuses, "s").status(), Status::UpToDate);
    }
}
