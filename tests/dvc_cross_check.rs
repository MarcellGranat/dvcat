//! Does our stale detection agree with `dvc status --json`?
//!
//! Needs a `dvc` executable: set `DVC_BIN=/path/to/dvc`, or have `dvc` on `PATH`.
//! Without one the test prints a notice and passes vacuously, so run it where DVC exists.
//!
//! `dvc status` judges every stage by its *own* inputs, while the app also marks downstream
//! stages stale. So we compare the set of stages that have an own reason (anything but `upstream`).

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use dvc_tui::{
    graph,
    project::Project,
    stale::{self, Reason},
};

fn find_dvc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("DVC_BIN") {
        return Some(PathBuf::from(path));
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join("dvc"))
        .find(|candidate| candidate.is_file())
}

fn copy_dir(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn replace_in(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(
        text.contains(from),
        "{from:?} not found in {}",
        path.display()
    );
    fs::write(path, text.replace(from, to)).unwrap();
}

/// Stages that `dvc status --json` reports as changed.
fn dvc_says_changed(dvc: &Path, dir: &Path) -> BTreeSet<String> {
    let output = Command::new(dvc)
        .args(["status", "--json"])
        .current_dir(dir)
        .env("DVC_NO_ANALYTICS", "1")
        .output()
        .expect("failed to run dvc");
    assert!(
        output.status.success(),
        "dvc status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("dvc status --json output");
    json.as_object().unwrap().keys().cloned().collect()
}

/// Stages with at least one reason of their own.
fn we_say_changed(dir: &Path) -> BTreeSet<String> {
    let project = Project::load(dir).unwrap();
    let graph = graph::build(&project.stages).unwrap();
    let statuses = stale::compute(&project, &graph);
    project
        .stages
        .iter()
        .zip(&statuses)
        .filter(|(_, status)| {
            status.reasons.iter().any(|r| {
                !matches!(
                    r,
                    Reason::Upstream { .. } | Reason::UpstreamUnverified { .. }
                )
            })
        })
        .map(|(stage, _)| stage.name.clone())
        .collect()
}

type Mutation = fn(&Path);

#[test]
fn our_verdicts_agree_with_dvc_status() {
    let Some(dvc) = find_dvc() else {
        eprintln!("SKIPPED: no dvc found (set DVC_BIN or put dvc on PATH)");
        return;
    };
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo");

    let scenarios: [(&str, Mutation); 6] = [
        ("untouched", |_| {}),
        ("param edited", |d| {
            replace_in(&d.join("params.yaml"), "lr: 0.01", "lr: 0.05")
        }),
        ("dependency edited", |d| {
            fs::write(d.join("src/features.py"), "# changed\n").unwrap();
        }),
        ("dependency deleted", |d| {
            fs::remove_file(d.join("data/raw.csv")).unwrap()
        }),
        ("cmd edited", |d| {
            replace_in(
                &d.join("dvc.yaml"),
                "python3 src/train.py",
                "python3 src/train.py --fast",
            );
        }),
        ("two stages edited", |d| {
            fs::write(d.join("src/prepare.py"), "# changed\n").unwrap();
            replace_in(&d.join("params.yaml"), "scale: 2.0", "scale: 3.0");
        }),
    ];

    let mut mismatches = Vec::new();
    for (name, mutate) in scenarios {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&demo, dir.path());
        mutate(dir.path());
        let (ours, theirs) = (
            we_say_changed(dir.path()),
            dvc_says_changed(&dvc, dir.path()),
        );
        println!("{name:20} ours={ours:?} dvc={theirs:?}");
        if ours != theirs {
            mismatches.push(format!("{name}: ours={ours:?} dvc={theirs:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "disagreements with dvc:\n{}",
        mismatches.join("\n")
    );
}
