//! Small helpers for editing `dvc.yaml` as text, so comments and layout survive, and for listing
//! the project's files.

use std::{fs, path::Path};

/// Directories never worth looking into.
const SKIPPED_DIRS: [&str; 4] = ["target", "node_modules", "__pycache__", "venv"];
const MAX_DEPTH: usize = 8;
/// Indent used for a section that has no entries yet.
const DEFAULT_INDENT: usize = 2;

/// Quote a YAML scalar only when it needs it.
pub fn scalar(text: &str) -> String {
    let plain = !text.is_empty()
        && text == text.trim()
        && !text.starts_with([
            '-', '?', '!', '&', '*', '|', '>', '%', '@', '`', '\'', '"', '#', '[', '{',
        ])
        && !text.contains([':', '#', ',', ']', '}'])
        && !matches!(text, "true" | "false" | "null" | "yes" | "no" | "~");
    if plain {
        text.to_owned()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// `text` with an entry added to the end of the top-level section `key:` (created at the end of
/// the file when missing). `entry` writes the entry for a given indent; `is_entry` tells which
/// line of the section is an entry's first line, to copy its indent. Nothing else is touched.
pub fn insert_into_section(
    text: &str,
    key: &str,
    entry: impl Fn(usize) -> String,
    is_entry: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let header = format!("{key}:");
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.starts_with(&header)) else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&header);
        out.push('\n');
        out.push_str(&entry(DEFAULT_INDENT));
        return Ok(out);
    };
    if lines[start].trim_end() != header {
        return Err(format!(
            "`{key}:` in dvc.yaml is written on one line; edit it by hand"
        ));
    }
    // The section runs while lines are indented, list items, comments or blank.
    let belongs = |l: &&&str| l.trim().is_empty() || l.starts_with([' ', '\t', '-', '#']);
    let end = lines[start + 1..].iter().take_while(belongs).count() + start + 1;
    // Insert after the last real line, so blank lines before the next key stay where they are.
    let at = (start + 1..end)
        .rev()
        .find(|&i| !lines[i].trim().is_empty() && !lines[i].trim_start().starts_with('#'))
        .map_or(start + 1, |i| i + 1);
    let indent = lines[start + 1..end]
        .iter()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .find(|l| is_entry(l))
        .map_or(DEFAULT_INDENT, |l| l.len() - l.trim_start().len());
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    for (offset, line) in entry(indent).lines().enumerate() {
        out.insert(at + offset, line.to_owned());
    }
    Ok(out.join("\n") + "\n")
}

/// Files below `root` for which `keep(file name, path)` holds, as paths relative to `root`, sorted.
/// Hidden directories (`.git`, `.dvc`, `.venv`) and build directories are not entered.
pub fn iter_files(root: &Path, keep: impl Fn(&str, &Path) -> bool) -> impl Iterator<Item = String> {
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut found: Vec<String> = Vec::new();
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_DEPTH
                    && !name.starts_with('.')
                    && !SKIPPED_DIRS.contains(&name.as_str())
                {
                    pending.push((path, depth + 1));
                }
            } else if kind.is_file()
                && keep(&name, &path)
                && let Ok(relative) = path.strip_prefix(root)
            {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found.into_iter()
}
