//! Build the Git + DVC demo with `make-git-demo.sh` and read its experiments back through our code.
//!
//! Needs DVC: set `DVC_BIN=/path/to/dvc` (or have `dvc` on `PATH`). Without it the test prints
//! `SKIPPED` and passes. Takes a few seconds, because it really runs three experiments.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use dvcat::experiments::{self, Column, RowKind};

fn find_dvc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("DVC_BIN") {
        return Some(PathBuf::from(path));
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join("dvc"))
        .find(|candidate| candidate.is_file())
}

#[test]
fn git_demo_has_three_experiments_that_the_table_can_read() {
    let Some(dvc) = find_dvc() else {
        eprintln!("SKIPPED: no dvc executable (set DVC_BIN)");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo/make-git-demo.sh");
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("demo");

    let output = Command::new(&script)
        .arg(&dest)
        .env("DVC", &dvc)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "script failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let table = experiments::fetch(&dvc, &dest).expect("exp show works in a Git project");
    assert!(table.has_experiments());
    let mut names: Vec<&str> = table
        .rows
        .iter()
        .filter(|row| row.kind == RowKind::Experiment)
        .map(|row| row.label.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["lr-001", "lr-005", "lr-010"]);
    let columns = table.columns();
    assert!(
        columns
            .iter()
            .any(|c| matches!(c, Column::Metric(m) if m.ends_with("auc")))
    );
    assert!(
        columns
            .iter()
            .any(|c| matches!(c, Column::Param(p) if p.ends_with("train.lr")))
    );
}
