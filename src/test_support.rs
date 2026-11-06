//! Helpers shared by unit tests. Compiled only for `cargo test`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::{app::App, graph, project::Project, stale};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn app_at(root: &Path) -> App {
    let project = Project::load(root).unwrap();
    let graph = graph::build(&project.stages).unwrap();
    App::new(project, graph)
}

/// The three-stage demo project: prepare -> features -> train.
pub fn demo_app() -> App {
    app_at(&fixture("demo"))
}

/// A throwaway project made of the given `(file name, contents)` pairs.
/// Keep the returned `TempDir` alive as long as the app is used: dropping it deletes the files.
pub fn app_with_files(files: &[(&str, &str)]) -> (TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    for (name, contents) in files {
        fs::write(dir.path().join(name), contents).unwrap();
    }
    let app = app_at(dir.path());
    (dir, app)
}

/// Recursively copy `src` into the (existing) directory `dst`.
pub fn copy_dir(src: &Path, dst: &Path) {
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

/// Fill in the stale-check result synchronously (the real app does this on a background thread).
pub fn with_statuses(mut app: App) -> App {
    app.statuses = Some(stale::compute(&app.project, &app.graph));
    app
}
