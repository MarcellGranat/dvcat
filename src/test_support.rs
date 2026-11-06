//! Helpers shared by unit tests. Compiled only for `cargo test`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::{app::App, graph, project::Project};

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
