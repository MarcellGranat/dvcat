//! The files a stage reads and writes, and a bounded preview of one of them.

use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use crate::project::Stage;

/// Most bytes read for a preview. A multi-gigabyte CSV must not freeze the UI.
pub const PREVIEW_LIMIT: usize = 64 * 1024;
/// Most directory entries listed in a preview.
const DIR_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Dep,
    Param,
    Out,
    Metric,
}

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dep => "dep",
            Self::Param => "param",
            Self::Out => "out",
            Self::Metric => "metric",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
}

/// Deps, param files, outs and metrics of a stage, in that order, each path once.
pub fn stage_files(stage: &Stage) -> Vec<FileEntry> {
    let candidates = stage
        .deps
        .iter()
        .map(|path| (path, FileKind::Dep))
        .chain(stage.params.iter().map(|p| (&p.file, FileKind::Param)))
        .chain(stage.outs.iter().map(|path| (path, FileKind::Out)))
        .chain(stage.metrics.iter().map(|path| (path, FileKind::Metric)));
    let mut entries: Vec<FileEntry> = Vec::new();
    for (path, kind) in candidates {
        if !entries.iter().any(|entry| &entry.path == path) {
            entries.push(FileEntry {
                path: path.clone(),
                kind,
            });
        }
    }
    entries
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewBody {
    Text { text: String, truncated: bool },
    Directory { names: Vec<String>, truncated: bool },
    Binary { bytes: u64 },
    Missing,
    Unreadable(String),
}

/// An open preview: what is shown and how far it is scrolled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub path: String,
    pub body: PreviewBody,
    /// First visible line.
    pub scroll: usize,
}

impl Preview {
    pub fn new(path: String, body: PreviewBody) -> Self {
        Self {
            path,
            body,
            scroll: 0,
        }
    }

    /// How many lines the body has when drawn.
    pub fn line_count(&self) -> usize {
        match &self.body {
            PreviewBody::Text { text, .. } => text.lines().count(),
            PreviewBody::Directory { names, .. } => names.len(),
            _ => 1,
        }
    }
}

/// Read at most [`PREVIEW_LIMIT`] bytes of `root/relative`.
pub fn load(root: &Path, relative: &str) -> PreviewBody {
    let path = root.join(relative);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return PreviewBody::Missing,
        Err(error) => return PreviewBody::Unreadable(error.to_string()),
    };
    if metadata.is_dir() {
        return load_directory(&path);
    }

    let mut bytes = Vec::new();
    // `take` caps the read: one byte more than the limit tells us the file was cut.
    let read = File::open(&path)
        .and_then(|file| file.take(PREVIEW_LIMIT as u64 + 1).read_to_end(&mut bytes));
    if let Err(error) = read {
        return PreviewBody::Unreadable(error.to_string());
    }
    let truncated = bytes.len() > PREVIEW_LIMIT;
    bytes.truncate(PREVIEW_LIMIT);
    // Text files never contain NUL; most binary formats do within their first bytes.
    if bytes.contains(&0) {
        return PreviewBody::Binary {
            bytes: metadata.len(),
        };
    }
    // `lossy` replaces invalid UTF-8 (including a character cut in half at the limit) with `�`.
    let text = String::from_utf8_lossy(&bytes)
        .replace('\r', "")
        .replace('\t', "    ");
    PreviewBody::Text { text, truncated }
}

fn load_directory(path: &Path) -> PreviewBody {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => return PreviewBody::Unreadable(error.to_string()),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    let truncated = names.len() > DIR_LIMIT;
    names.truncate(DIR_LIMIT);
    PreviewBody::Directory { names, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{project::ParamRef, test_support::fixture};

    fn stage() -> Stage {
        Stage {
            name: "train".into(),
            cmd: String::new(),
            deps: vec!["src/train.py".into(), "data/features.csv".into()],
            params: vec![ParamRef {
                file: "params.yaml".into(),
                keys: vec!["train.lr".into()],
            }],
            outs: vec!["model.txt".into()],
            metrics: vec!["metrics.json".into(), "model.txt".into()],
            unsupported: None,
        }
    }

    #[test]
    fn lists_deps_params_outs_and_metrics_each_path_once() {
        let files = stage_files(&stage());
        let seen: Vec<(&str, FileKind)> = files.iter().map(|e| (e.path.as_str(), e.kind)).collect();
        assert_eq!(
            seen,
            [
                ("src/train.py", FileKind::Dep),
                ("data/features.csv", FileKind::Dep),
                ("params.yaml", FileKind::Param),
                ("model.txt", FileKind::Out),
                ("metrics.json", FileKind::Metric),
            ]
        );
    }

    #[test]
    fn text_files_are_read_with_tabs_expanded() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "one\r\n\ttwo\n").unwrap();
        assert_eq!(
            load(dir.path(), "a.txt"),
            PreviewBody::Text {
                text: "one\n    two\n".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn big_files_are_cut_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("big.txt"), "x".repeat(PREVIEW_LIMIT + 10)).unwrap();
        let PreviewBody::Text { text, truncated } = load(dir.path(), "big.txt") else {
            panic!("expected text");
        };
        assert!(truncated);
        assert_eq!(text.len(), PREVIEW_LIMIT);
    }

    #[test]
    fn binary_missing_and_directory_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.bin"), [1u8, 0, 2]).unwrap();
        fs::create_dir(dir.path().join("d")).unwrap();
        fs::write(dir.path().join("d/f.txt"), "x").unwrap();
        fs::create_dir(dir.path().join("d/sub")).unwrap();
        assert_eq!(load(dir.path(), "b.bin"), PreviewBody::Binary { bytes: 3 });
        assert_eq!(load(dir.path(), "nope"), PreviewBody::Missing);
        assert_eq!(
            load(dir.path(), "d"),
            PreviewBody::Directory {
                names: vec!["f.txt".into(), "sub/".into()],
                truncated: false
            }
        );
    }

    #[test]
    fn demo_params_file_can_be_previewed() {
        let PreviewBody::Text { text, .. } = load(&fixture("demo"), "params.yaml") else {
            panic!("expected text");
        };
        assert!(text.contains("lr"), "{text}");
    }

    #[test]
    fn line_count_depends_on_the_body() {
        let text = |t: &str| PreviewBody::Text {
            text: t.into(),
            truncated: false,
        };
        assert_eq!(Preview::new("a".into(), text("1\n2\n3\n")).line_count(), 3);
        assert_eq!(
            Preview::new("a".into(), PreviewBody::Missing).line_count(),
            1
        );
    }
}
